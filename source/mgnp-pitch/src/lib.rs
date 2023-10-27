#![feature(async_fn_in_trait)]
#![cfg_attr(not(test), no_std)]

use conn_table::ConnTable;
pub use conn_table::{Id, LinkId};
use futures::FutureExt;
use uuid::Uuid;

mod conn_table;

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
    Control(ControlMessage),
    Data {
        local_id: Id,
        remote_id: Id,
        data: &'data [u8],
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ControlMessage {
    Ack { local_id: Id, remote_id: Id },
    Nak { remote_id: Id },
    // TODO(eliza): this needs some kind of identity for registry lookups...
    Connect { local_id: Id },
    Reset { remote_id: Id },
}

pub struct Interface<Fr, Wi, const MAX_CONNS: usize = { DEFAULT_MAX_CONNS }>
where
    Fr: Frame,
    // Remote wire type
    Wi: Wire<Frame = Fr>,
{
    wire: Wi,
    conn_table: ConnTable<MAX_CONNS>,
}

struct Identity {
    id: Uuid,
    kind: IdentityKind,
}

enum IdentityKind {
    Name(heapless::String<32>),
}

struct Registry {}

pub const DEFAULT_MAX_CONNS: usize = 512;

impl<Fr, Wi, const MAX_CONNS: usize> Interface<Fr, Wi, MAX_CONNS>
where
    Fr: Frame,
    Wi: Wire<Frame = Fr>,
{
    pub fn new(wire: Wi) -> Self {
        Self {
            wire,
            conn_table: ConnTable::new(),
        }
    }

    pub async fn handle_connection(&mut self) -> Result<(), ()> {
        let Self { wire, conn_table } = self;
        loop {
            let frame = futures::select_biased! {
                // inbound frame from the wire.
                frame = wire.recv().fuse() => frame?,
                // either a connection needs to send data, or a connection has
                // closed locally.
                frame = conn_table.next_outbound().fuse() => {
                    wire.send(frame).await?;
                    continue;
                },
                // OR handle local conn request (need to figure out API for this...)
            };

            let msg = match frame.decode() {
                Ok(msg) => msg,
                Err(error) => {
                    tracing::warn!(%error, "failed to decode inbound frame");
                    continue;
                }
            };
            let id = msg.link_id();
            if id == LinkId::INTERFACE {
                todo!("eliza: handle interface frame");
            } else {
                // frame is local
                if let Err(error) = conn_table.process_inbound(msg).await {
                    tracing::debug!(%id, %error, "resetting connection");
                    // link does not exist, reject
                    let reset = Message::Control(ControlMessage::Reset {
                        remote_id: id.remote.unwrap(),
                    });
                    wire.send(reset).await?;
                }
            }
        }
    }
}

impl Message<'_> {
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

impl ControlMessage {
    fn link_id(&self) -> LinkId {
        match *self {
            Self::Ack {
                local_id,
                remote_id,
            } => LinkId {
                local: Some(local_id),
                remote: Some(remote_id),
            },
            Self::Nak { remote_id } => LinkId {
                local: None,
                remote: Some(remote_id),
            },
            Self::Connect { local_id } => LinkId {
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
