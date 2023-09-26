#![feature(async_fn_in_trait)]

use conn_table::ConnTable;
use uuid::Uuid;
mod conn_table;

pub trait Frame {
    fn as_bytes(&self) -> &[u8];
}

pub trait Wire {
    type F: Frame;
    async fn send(&self, f: Self::F) -> Result<(), ()>;
    async fn recv(&self) -> Result<Self::F, ()>;
}

pub struct Interface<Fr, Wi>
where
    Fr: Frame,
    Wi: Wire<F = Fr>,
{
    wire: Wi,
    conn_table: ConnTable<(), CONN_TABLE_CAPACITY>,
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
