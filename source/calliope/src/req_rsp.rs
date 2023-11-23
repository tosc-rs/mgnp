use crate::{client, message, Service};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use futures::{future::FutureExt, pin_mut, select_biased};
use maitake_sync::{
    wait_map::{WaitError, WaitMap, WakeOutcome},
    WaitCell,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tracing::{instrument, Level};
use tricky_pipe::mpsc::error::{RecvError, SendError, TrySendError};

#[cfg(test)]
mod tests;

#[derive(Debug, Eq, PartialEq)]
pub struct Seq(usize);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[must_use]
pub struct Request<T> {
    seq: usize,
    body: T,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[must_use]
pub struct Response<T> {
    seq: usize,
    body: T,
}

pub struct Client<S>
where
    S: ReqRspService,
{
    seq: AtomicUsize,
    channel: client::Connection<S>,
    shutdown: WaitCell,
    dispatcher: WaitMap<usize, S::Response>,
    has_dispatcher: AtomicBool,
}

pub trait ReqRspService:
    Service<ClientMsg = Request<Self::Request>, ServerMsg = Response<Self::Response>>
{
    type Request: Serialize + DeserializeOwned + Send + Sync + 'static;
    type Response: Serialize + DeserializeOwned + Send + Sync + 'static;
}

// === impl Seq ===

impl Seq {
    pub fn respond<T>(self, body: T) -> Response<T> {
        Response { seq: self.0, body }
    }
}

// === impl Request ===

impl<T> Request<T> {
    pub fn body(&self) -> &T {
        &self.body
    }

    pub fn into_parts(self) -> (Seq, T) {
        (Seq(self.seq), self.body)
    }

    pub fn respond<U>(self, body: U) -> Response<U> {
        Response {
            seq: self.seq,
            body,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DispatchError {
    AlreadyRunning,
    ConnectionReset(message::Reset),
}

// === impl Client ===

impl<S> Client<S>
where
    S: ReqRspService,
{
    pub fn new(client: client::Connection<S>) -> Self {
        Self {
            seq: AtomicUsize::new(0),
            channel: client,
            dispatcher: WaitMap::new(),
            has_dispatcher: AtomicBool::new(false),
            shutdown: WaitCell::new(),
        }
    }

    pub async fn request(&self, body: S::Request) -> Result<S::Response, message::Reset> {
        #[cfg_attr(debug_assertions, allow(unreachable_code))]
        let handle_wait_error = |err: WaitError| -> message::Reset {
            match err {
                WaitError::Closed => {
                    let error = self.channel.tx().try_reserve().expect_err("if the waitmap was closed, then the channel should have been closed with an error!");
                    if let TrySendError::Error { error, .. } = error {
                        return error;
                    }

                    #[cfg(debug_assertions)]
                    unreachable!("closing the channel with an error should have priority over full/disconnected errors.");

                    message::Reset::BecauseISaidSo
                }
                WaitError::Duplicate => panic!("sequence number was reused, implying we overflowed a usize! this is real bad news..."),
                WaitError::AlreadyConsumed => unreachable!("data should not already be consumed, this is a bug"),
                WaitError::NeverAdded => unreachable!("we ensured the waiter was added, this is a bug!"),
                error => {
                    #[cfg(debug_assertions)]
                    todo!("james added a new WaitError variant that we don't know how to handle: {error:}");

                    #[cfg_attr(debug_assertions, allow(unreachable_code))]
                    message::Reset::BecauseISaidSo
                }
            }
        };

        // aquire a send permit first --- this way, we don't increment the
        // sequence number until we actually have a channel reservation.
        let permit = self.channel.tx().reserve().await.map_err(|e| match e {
            SendError::Disconnected(()) => message::Reset::BecauseISaidSo,
            SendError::Error { error, .. } => error,
        })?;
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let req = Request { seq, body };

        // ensure waiter is enqueued before sending the request.
        let wait = self.dispatcher.wait(seq);
        pin_mut!(wait);
        wait.as_mut().enqueue().await.map_err(handle_wait_error)?;

        // actually send the message...
        permit.send(req);

        wait.await.map_err(handle_wait_error)
    }

    /// Shut down the client dispatcher for this `Client`.
    ///
    /// This will fail any outstanding `Request` futures, and reset the
    /// connection.
    pub fn shutdown(&self) {
        tracing::debug!("shutting down client...");
        self.shutdown.close();
        self.channel
            .close_with_error(message::Reset::BecauseISaidSo);
        self.dispatcher.close();
    }

    /// Run the client's dispatcher in the background until cancelled or the
    /// connection is reset.
    #[instrument(
        level = Level::DEBUG,
        name = "Client::dispatcher",
        skip(self),
        fields(svc = %core::any::type_name::<S>()),
        ret(Debug),
        err(Debug),
    )]
    pub async fn dispatch(&self) -> Result<(), DispatchError> {
        #[cfg_attr(debug_assertions, allow(unreachable_code))]
        if self.has_dispatcher.swap(true, Ordering::AcqRel) {
            #[cfg(debug_assertions)]
            panic!(
                "a client connection may only have one running dispatcher \
                task! a second call to `Client::dispatch` is likely a bug. \
                this is a panic in debug mode."
            );

            tracing::warn!("a client connection may only have one running dispatcher task!");
            return Err(DispatchError::AlreadyRunning);
        }

        loop {
            // wait for the next server message, or for the client to trigger a
            // shutdown.
            let msg = select_biased! {
                _ = self.shutdown.wait().fuse() => {
                    tracing::debug!("client dispatcher `sshutting down...");
                    return Ok(());
                }
                msg = self.channel.rx().recv().fuse() => msg,
            };

            let Response { seq, body } = match msg {
                Ok(msg) => msg,
                Err(reset) => {
                    let reset = match reset {
                        RecvError::Error(e) => e,
                        _ => message::Reset::BecauseISaidSo,
                    };

                    tracing::debug!(%reset, "client connection reset, shutting down...");
                    self.channel.close_with_error(reset);
                    self.dispatcher.close();
                    return Err(DispatchError::ConnectionReset(reset));
                }
            };

            tracing::trace!(seq, "dispatching response...");

            match self.dispatcher.wake(&seq, body) {
                WakeOutcome::Woke => {
                    tracing::trace!(seq, "dispatched response");
                }
                WakeOutcome::Closed(_) => {
                    #[cfg(debug_assertions)]
                    unreachable!("the dispatcher should not be closed if it is still running...");
                }
                WakeOutcome::NoMatch(_) => {
                    tracing::debug!(seq, "client no longer interested in request");
                }
            };
        }
    }
}

impl<S, Req, Rsp> ReqRspService for S
where
    S: Service<ClientMsg = Request<Req>, ServerMsg = Response<Rsp>>,
    Req: Serialize + DeserializeOwned + Send + Sync + 'static,
    Rsp: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    type Request = Req;
    type Response = Rsp;
}
