#![feature(async_fn_in_trait)]
#![cfg_attr(not(test), no_std)]

use conn_table::ConnTable;
use futures::FutureExt;
use uuid::Uuid;

mod conn_table;

pub trait Frame {
    fn as_bytes(&self) -> &[u8];
    fn link_id(&self) -> LinkId;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct LinkId {
    pub local: Option<conn_table::Id>,
    pub remote: Option<conn_table::Id>,
}

pub trait Wire {
    type Frame: Frame;
    async fn send(&self, f: Self::Frame) -> Result<(), ()>;
    async fn recv(&self) -> Result<Self::Frame, ()>;
}

pub struct Interface<Fr, Wi, Lcl>
where
    Fr: Frame,
    // Remote wire type
    Wi: Wire<Frame = Fr>,
    // Local connection type
    Lcl: Wire<Frame = Fr>,
{
    wire: Wi,
    conn_table: ConnTable<Lcl, CONN_TABLE_CAPACITY>,
}

pub const CONN_TABLE_CAPACITY: usize = 512;

struct Identity {
    id: Uuid,
    kind: IdentityKind,
}

enum IdentityKind {
    Name(heapless::String<32>),
}

struct Registry {}

impl<Fr, Wi, Lcl> Interface<Fr, Wi, Lcl>
where
    Fr: Frame,
    Wi: Wire<Frame = Fr>,
    Lcl: Wire<Frame = Fr>,
{
    pub async fn handle_connection(&mut self) -> Result<(), ()> {
        let Self { wire, conn_table } = self;
        loop {
            let frame = futures::select_biased! {
                // inbound frame from the wire.
                frame = wire.recv().fuse() => frame?,
                // either a connection needs to send data, or a connection has
                // closed locally.
                frame = conn_table.next_outbound().fuse() => {
                    let frame: Fr = todo!("eliza: serialize outbound msg to frame; may need interface changes...");
                    self.wire.send(frame).await?;
                    continue;
                },
                // OR handle local conn request (need to figure out API for this...)
            };

            let id = frame.link_id();
            if id == LinkId::INTERFACE {
                todo!("eliza: handle interface frame");
            } else {
                // frame is local
                if let Err(e) = conn_table.process_inbound(frame).await {
                    // link does not exist, reject
                    let reset = todo!("eliza: generate resets...")
                    wire.send(reset).await;
                }
            }
        }
    }
}

impl LinkId {
    pub const INTERFACE: Self = Self {
        local: None,
        remote: None,
    };
}
