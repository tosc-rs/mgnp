use super::Nak;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tricky_pipe::bidi::SerBiDi;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    pub id: Uuid,
    pub kind: IdentityKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum IdentityKind {
    Name(heapless::String<32>),
}

/// Represents a mechanism for discovering services on the local node.
pub trait Registry {
    async fn connect(&self, identity: Identity, hello: &[u8]) -> Result<SerBiDi, Nak>;
}

pub trait Service {
    const UUID: Uuid;
    type Hello: Serialize + DeserializeOwned + Send + Sync + 'static;
    type ConnectError: Serialize + DeserializeOwned + Send + Sync + 'static;
    type ClientMsg: Serialize + DeserializeOwned + Send + Sync + 'static;
    type ServerMsg: Serialize + DeserializeOwned + Send + Sync + 'static;
}
