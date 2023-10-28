#![feature(async_fn_in_trait)]
#![cfg_attr(not(test), no_std)]

use conn_table::ConnTable;
pub use conn_table::{Id, LinkId};
use futures::{FutureExt, Stream, StreamExt};
use tricky_pipe::bidi::SerBiDi;

mod conn_table;
pub mod registry;
pub use registry::Registry;

pub trait Frame {
    fn as_bytes(&self) -> &[u8];

    fn decode(&self) -> postcard::Result<Message<'_>> {
        postcard::from_bytes(self.as_bytes())
    }
}

pub trait Wire {
    type Frame: Frame;
    async fn send(&self, f: Message<'_>) -> Result<(), ()>;
    async fn recv(&self) -> Result<Self::Frame, ()>;
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Message<'data> {
    Control(ControlMessage<'data>),
    Data {
        local_id: Id,
        remote_id: Id,
        data: &'data [u8],
    },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ControlMessage<'data> {
    Ack {
        local_id: Id,
        remote_id: Id,
    },
    Nak {
        remote_id: Id,
        reason: Nak,
    },
    Connect {
        local_id: Id,
        identity: registry::Identity,
        hello: &'data [u8],
    },
    Reset {
        remote_id: Id,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Nak {
    ConnTableFull(usize),
    NotFound,
    Rejected(
        // TODO(eliza): can we cram a serialized message into this...?
    ),
}

pub struct Interface<Fr, Wi, R, const MAX_CONNS: usize = { DEFAULT_MAX_CONNS }>
where
    Fr: Frame,
    // Remote wire type
    Wi: Wire<Frame = Fr>,
{
    wire: Wi,
    conn_table: ConnTable<MAX_CONNS>,
    registry: R,
}

/// An outbound connect request.
pub struct OutboundConnect<'data> {
    /// The identity of the remote service to connect to.
    pub identity: registry::Identity,
    /// The "hello" message to send to the remote service.
    pub hello: &'data [u8],
    /// The local bidirectional channel to bind to the remote service.
    pub channel: SerBiDi,

    // TODO(eliza): add a oneshot for "connect success/connect failed"...
    _p: (),
}

pub const DEFAULT_MAX_CONNS: usize = 512;

impl<Fr, Wi, R, const MAX_CONNS: usize> Interface<Fr, Wi, R, MAX_CONNS>
where
    Fr: Frame,
    Wi: Wire<Frame = Fr>,
    R: Registry,
{
    pub fn new(wire: Wi, registry: R) -> Self {
        Self {
            wire,
            conn_table: ConnTable::new(),
            registry,
        }
    }

    pub async fn run(&mut self, conns: impl Stream<Item = OutboundConnect<'_>>) -> Result<(), ()> {
        futures::pin_mut!(conns);
        loop {
            futures::select_biased! {
                // inbound frame from the wire.
                frame = self.wire.recv().fuse() => self.process_inbound(frame?).await?,
                // either a connection needs to send data, or a connection has
                // closed locally.
                frame = self.conn_table.next_outbound().fuse() => {
                    self.wire.send(frame).await?;
                },

                // locally-initiated connect request
                conn = conns.next().fuse() => {
                    let Some(OutboundConnect { identity, hello, channel, .. }) = conn else {
                        tracing::info!("connection stream has terminated");
                        // TODO(eliza): should the run loop die here instead?
                        continue;
                    };

                    tracing::debug!(?identity, "initiating local connection...");
                    match self.conn_table.start_connecting(identity, hello, channel) {
                        Some(frame) => {
                            self.wire.send(Message::Control(frame)).await?;
                            tracing::debug!("local connect sent!")
                        }
                        None => {
                            tracing::info!("refusing connection; no space in conn table!")
                            // TODO(eliza): tell the client that they can't have
                            // what hey want...
                        },
                    }

                }
            };
        }
    }

    async fn process_inbound(&mut self, frame: Fr) -> Result<(), ()> {
        let msg = match frame.decode() {
            Ok(msg) => msg,
            Err(error) => {
                tracing::warn!(%error, "failed to decode inbound frame");
                // return "Ok" here, even though there was an error, because we
                // want the interface loop to keep running, instead of dying.
                return Ok(());
            }
        };

        let id = msg.link_id();
        if id == LinkId::INTERFACE {
            todo!("eliza: handle interface frame");
            // return Ok(());
        }

        // process the inbound message, possibly generating a new outbound frame
        // to send.
        if let Some(rsp) = self.conn_table.process_inbound(&self.registry, msg).await {
            self.wire.send(rsp).await?;
        }

        Ok(())
    }
}

impl Message<'_> {
    pub(crate) fn reset(remote_id: Id) -> Self {
        Self::Control(ControlMessage::Reset { remote_id })
    }

    fn link_id(&self) -> LinkId {
        match self {
            Self::Control(msg) => msg.link_id(),
            Self::Data {
                local_id,
                remote_id,
                ..
            } => LinkId {
                local: Some(*local_id),
                remote: Some(*remote_id),
            },
        }
    }
}

impl ControlMessage<'_> {
    fn link_id(&self) -> LinkId {
        match *self {
            Self::Ack {
                local_id,
                remote_id,
            } => LinkId {
                local: Some(local_id),
                remote: Some(remote_id),
            },
            Self::Nak { remote_id, .. } => LinkId {
                local: None,
                remote: Some(remote_id),
            },
            Self::Connect { local_id, .. } => LinkId {
                local: Some(local_id),
                remote: None,
            },
            Self::Reset { remote_id } => LinkId {
                remote: Some(remote_id),
                local: None,
            },
        }
    }
}
