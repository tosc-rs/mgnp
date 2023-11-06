use crate::{channel, message::Nak, registry};
use tricky_pipe::{bidi, mpsc, oneshot, serbox};

pub struct Connector<S: registry::Service> {
    hello_sharer: serbox::Sharer<S::Hello>,
    rsp: oneshot::Receiver<Result<channel::Ids, Nak>>,
    tx: mpsc::Sender<OutboundConnect>,
}

/// An outbound connect request.
pub struct OutboundConnect {
    /// The identity of the remote service to connect to.
    identity: registry::Identity,
    /// The "hello" message to send to the remote service.
    hello: serbox::Consumer,
    /// The local bidirectional channel to bind to the remote service.
    channel: bidi::SerBiDi,
    /// Sender for the response from the remote service.
    rsp: oneshot::Sender<Result<(), Nak>>,
}

pub enum ConnectError {
    InterfaceDead,
    Nak(Nak),
}

impl<S: registry::Service> Connector<S> {
    pub async fn connect(
        &mut self,
        identity: registry::Identity,
        hello: S::Hello,
        srv_chan: bidi::SerBiDi,
        client_chan: bidi::BiDi<S::ServerMsg, channel::DataFrame<S::ClientMsg>>,
    ) -> Result<channel::ClientChannel<S>, ConnectError> {
        let permit = self
            .tx
            .reserve()
            .await
            .map_err(|_| ConnectError::InterfaceDead)?;
        let hello = self.hello_sharer.share(hello).await;
        let rsp = todo!("eliza: redesign oneshot interface :(");
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
