use crate::{
    message::{Rejection, Reset},
    req_rsp, service, Service,
};
use tricky_pipe::{bidi, mpsc, oneshot, serbox};

pub struct Connector<S: Service> {
    pub(super) hello_sharer: serbox::Sharer<S::Hello>,
    pub(super) rsp: oneshot::Receiver<Result<(), Rejection>>,
    pub(super) tx: mpsc::Sender<OutboundConnect>,
}

/// An outbound connect request.
pub struct OutboundConnect {
    /// The identity of the remote service to connect to.
    pub(crate) identity: service::Identity,
    /// The "hello" message to send to the remote service.
    pub(crate) hello: serbox::Consumer,
    /// The local bidirectional channel to bind to the remote service.
    pub(crate) channel: bidi::SerBiDi<Reset>,
    /// Sender for the response from the remote service.
    pub(crate) rsp: oneshot::Sender<Result<(), Rejection>>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ConnectError {
    InterfaceDead,
    Nak(Rejection),
}

pub type Connection<S> = bidi::BiDi<<S as Service>::ServerMsg, <S as Service>::ClientMsg, Reset>;

pub struct Channels<S: Service> {
    srv_chan: bidi::SerBiDi<Reset>,
    client_chan: bidi::BiDi<S::ServerMsg, S::ClientMsg, Reset>,
}

pub struct StaticChannels<S: Service, const CAPACITY: usize> {
    s2c: mpsc::StaticTrickyPipe<S::ServerMsg, CAPACITY>,
    c2s: mpsc::StaticTrickyPipe<S::ClientMsg, CAPACITY>,
}

impl<S: Service> Channels<S> {
    pub fn from_static<const CAPACITY: usize>(
        storage: &'static StaticChannels<S, CAPACITY>,
    ) -> Self {
        todo!("eliza")
    }

    #[cfg(any(test, feature = "alloc"))]
    pub fn new(capacity: u8) -> Self {
        let s2c = mpsc::TrickyPipe::new(capacity);
        let c2s = mpsc::TrickyPipe::new(capacity);
        let srv_chan = bidi::SerBiDi::from_pair(c2s.deser_sender(), s2c.ser_receiver().unwrap());
        let client_chan = bidi::BiDi::from_pair(s2c.sender(), c2s.receiver().unwrap());
        Self {
            srv_chan,
            client_chan,
        }
    }
}

impl<S: Service> Connector<S> {
    pub async fn connect(
        &mut self,
        identity: impl Into<service::IdentityKind>,
        hello: S::Hello,
        Channels {
            srv_chan,
            client_chan,
        }: Channels<S>,
    ) -> Result<Connection<S>, ConnectError> {
        let permit = self
            .tx
            .reserve()
            .await
            .map_err(|_| ConnectError::InterfaceDead)?;
        let hello = self.hello_sharer.share(hello).await;
        let rsp = self.rsp.sender().await.unwrap();
        let connect = OutboundConnect {
            identity: service::Identity::new::<S>(identity.into()),
            hello,
            channel: srv_chan,
            rsp,
        };
        permit.send(connect);
        match self.rsp.recv().await {
            Err(_) => Err(ConnectError::InterfaceDead),
            Ok(Err(nak)) => Err(ConnectError::Nak(nak)),
            Ok(Ok(_)) => Ok(client_chan),
        }
    }
}

impl<S: req_rsp::ReqRspService> Connector<S> {
    pub async fn connect_req_rsp(
        &mut self,
        identity: impl Into<service::IdentityKind>,
        hello: S::Hello,
        channels: Channels<S>,
    ) -> Result<req_rsp::Client<S>, ConnectError> {
        self.connect(identity, hello, channels)
            .await
            .map(req_rsp::Client::new)
    }
}
