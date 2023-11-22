use super::{message::Reset, Rejection};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tricky_pipe::bidi::SerBiDi;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct Identity {
    id: Uuid,
    kind: IdentityKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum IdentityKind {
    Name(heapless::String<32>),
}

/// Represents a mechanism for discovering services on the local node.
pub trait Registry {
    async fn connect(&self, identity: Identity, hello: &[u8]) -> Result<SerBiDi<Reset>, Rejection>;
}

/// A service definition.
///
/// This trait defines the interface for a MGNP service.
pub trait Service {
    /// The [universally unique identifier] (UUID) that identifies this service
    /// interface definition.
    ///
    /// [universally unique identifier]: https://en.wikipedia.org/wiki/Universally_unique_identifier
    const UUID: Uuid;

    /// The message type sent by clients when initiating a new connection to
    /// this service.
    ///
    /// The `Hello` message may be used to determine the kind of connection
    /// established by the client, or be used by the service to route
    /// connections to different handlers or components. The service may also
    /// choose to reject a connection based on the value of the `Hello` message,
    /// returning a [`ConnectError`](Self::ConnectError).
    ///
    /// If the service does not require a `Hello` message when establishing a
    /// connection, this type may be [`()`].
    type Hello: Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Errors returned by the service if a new, incoming connection request is
    /// rejected.
    ///
    /// Services which do not reject incoming connections may set this type to
    /// [`core::convert::Infallible`].
    type ConnectError: Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Messages sent by clients to this service.
    type ClientMsg: Serialize + DeserializeOwned + Send + Sync + 'static;

    /// Messages sent by this service to clients.
    type ServerMsg: Serialize + DeserializeOwned + Send + Sync + 'static;
}

impl Identity {
    pub const fn new<S: Service>(kind: IdentityKind) -> Self {
        Self { id: S::UUID, kind }
    }

    pub fn from_name<S: Service>(name: impl Into<heapless::String<32>>) -> Self {
        Self::new::<S>(IdentityKind::Name(name.into()))
    }

    pub const fn uuid(&self) -> &Uuid {
        &self.id
    }

    pub const fn kind(&self) -> &IdentityKind {
        &self.kind
    }
}

impl From<&'_ str> for IdentityKind {
    fn from(s: &str) -> Self {
        IdentityKind::Name(s.into())
    }
}

impl From<heapless::String<32>> for IdentityKind {
    fn from(s: heapless::String<32>) -> Self {
        IdentityKind::Name(s)
    }
}
