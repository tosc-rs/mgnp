use crate::{channel, message::Nak, registry};
use tricky_pipe::{bidi, mpsc, oneshot, serbox};

pub struct Connector<S: registry::Service> {
    pub(super) hello_sharer: serbox::Sharer<S::Hello>,
    pub(super) rsp: oneshot::Receiver<Result<channel::Ids, Nak>>,
    pub(super) tx: mpsc::Sender<OutboundConnect>,
}

/// An outbound connect request.
pub struct OutboundConnect {
    /// The identity of the remote service to connect to.
    pub(crate) identity: registry::Identity,
    /// The "hello" message to send to the remote service.
    pub(crate) hello: serbox::Consumer,
    /// The local bidirectional channel to bind to the remote service.
    pub(crate) channel: bidi::SerBiDi,
    /// Sender for the response from the remote service.
    pub(crate) rsp: oneshot::Sender<Result<channel::Ids, Nak>>,
}

#[derive(Debug)]
pub enum ConnectError {
    InterfaceDead,
    Nak(Nak),
}

pub struct Channels<S: registry::Service> {
    srv_chan: bidi::SerBiDi,
    client_chan: bidi::BiDi<S::ServerMsg, channel::DataFrame<S::ClientMsg>>,
}

pub struct StaticChannels<S: registry::Service, const CAPACITY: usize> {
    s2c: tricky_pipe::StaticTrickyPipe<S::ServerMsg, CAPACITY>,
    c2s: tricky_pipe::StaticTrickyPipe<channel::DataFrame<S::ClientMsg>, CAPACITY>,
}

impl<S: registry::Service> Channels<S> {
    pub fn from_static<const CAPACITY: usize>(
        storage: &'static StaticChannels<S, CAPACITY>,
    ) -> Self {
        todo!("eliza")
    }

    #[cfg(any(test, feature = "alloc"))]
    pub fn new(capacity: u8) -> Self {
        use tricky_pipe::TrickyPipe;
        let s2c = TrickyPipe::new(capacity);
        let c2s = TrickyPipe::new(capacity);
        let srv_chan = bidi::SerBiDi::from_pair(c2s.deser_sender(), s2c.ser_receiver().unwrap());
        let client_chan = bidi::BiDi::from_pair(s2c.sender(), c2s.receiver().unwrap());
        Self {
            srv_chan,
            client_chan,
        }
    }
}

impl<S: registry::Service> Connector<S> {
    pub async fn connect(
        &mut self,
        identity: registry::Identity,
        hello: S::Hello,
        Channels {
            srv_chan,
            client_chan,
        }: Channels<S>,
    ) -> Result<channel::ClientChannel<S>, ConnectError> {
        let permit = self
            .tx
            .reserve()
            .await
            .map_err(|_| ConnectError::InterfaceDead)?;
        let hello = self.hello_sharer.share(hello).await;
        let rsp = self.rsp.sender().await.unwrap();
        let connect = OutboundConnect {
            identity,
            hello,
            channel: srv_chan,
            rsp,
        };
        permit.send(connect);
        match self.rsp.recv().await {
            Err(_) => Err(ConnectError::InterfaceDead),
            Ok(Err(nak)) => Err(ConnectError::Nak(nak)),
            Ok(Ok(ids)) => Ok(channel::ClientChannel::new(ids, client_chan)),
        }
    }
}
