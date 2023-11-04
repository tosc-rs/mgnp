use super::Nak;
use tricky_pipe::bidi::SerBiDi;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Identity {
    pub id: Uuid,
    pub kind: IdentityKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub enum IdentityKind {
    Name(heapless::String<32>),
}

/// Represents a mechanism for discovering services on the local node.
pub trait Registry {
    async fn connect(&self, identity: Identity, hello: &[u8]) -> Result<SerBiDi, Nak>;
}