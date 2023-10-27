use crate::{ControlMessage, Message};
use core::{fmt, mem, num::NonZeroU16};
use tricky_pipe::{bidi::SerBiDi, SerPermit};

#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Id(NonZeroU16);

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct LinkId {
    pub local: Option<Id>,
    pub remote: Option<Id>,
}

pub struct ConnTable<const CAPACITY: usize> {
    conns: Entries<CAPACITY>,
    next_id: Id,
    len: usize,
}

#[derive(Debug)]
pub enum State {
    Open { remote_id: Id },
    Connecting,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum InboundError {
    /// The connection tracking table doesn't have a connection for the provided ID.
    NoSocket,

    /// The connection was already acked by the remote, so they acked it again
    /// for some reason?
    AlreadyEstablished { remote_id: Id },

    /// The local channel for this socket has closed.
    ChannelClosed,

    /// The message could not be deserialized.
    Deserialize(postcard::Error),
}

#[derive(Debug)]
struct Socket {
    state: State,
    bidi: SerBiDi,
}

enum Entry {
    Unused,
    Closed(Id),
    Occupied(Socket),
}

/// Wrapper struct so we can have a `get_mut` that's indexed by `Id`, basically.
struct Entries<const CAPACITY: usize>([Entry; CAPACITY]);

impl<const CAPACITY: usize> ConnTable<CAPACITY> {
    const ENTRY_UNUSED: Entry = Entry::Unused;

    #[must_use]
    pub const fn new() -> Self {
        Self {
            conns: Entries([Self::ENTRY_UNUSED; CAPACITY]),
            // ID 0 is the link-control channel
            next_id: Id(unsafe { NonZeroU16::new_unchecked(1) }),
            len: 0,
        }
    }

    /// Returns the next outbound frame.
    pub async fn next_outbound(&mut self) -> Message<'_> {
        todo!()
    }

    /// Process an inbound frame.
    pub async fn process_inbound(&mut self, msg: Message<'_>) -> Result<(), InboundError> {
        let _span = tracing::trace_span!("process_inbound", id = %msg.link_id()).entered();
        match msg {
            Message::Data {
                local_id,
                remote_id,
                data,
            } => {
                tracing::trace!(
                    id.remote = %local_id,
                    id.local = %remote_id,
                    len = data.len(),
                    "process_inbound: data",
                );
                // the remote peer's remote ID is our local ID.
                let id = remote_id;
                let socket = self.conns.get_mut(id).ok_or(InboundError::NoSocket)?;

                // try to reserve send capacity on this socket.
                let error = match socket.reserve_send().await {
                    Ok(permit) => return permit.send(data).map_err(InboundError::Deserialize),
                    Err(error) => error,
                };

                // otherwise, we couldn't reserve a send permit because the
                // channel has closed locally.
                tracing::trace!("process_inbound: local error: {error}; resetting...");
                self.close(id);
                Err(error)
            }
            Message::Control(ControlMessage::Ack {
                local_id,
                remote_id,
            }) => {
                tracing::trace!(id.remote = %local_id, id.local = %remote_id, "process_inbound: ACK");
                self.process_ack(remote_id, local_id)
            }
            Message::Control(ControlMessage::Nak { remote_id }) => {
                tracing::trace!(id.local = %remote_id, "process_inbound: NAK");
                self.close(remote_id);
                Ok(())
            }
            Message::Control(ControlMessage::Reset { remote_id }) => {
                tracing::trace!(id.local = %remote_id, "process_inbound: RESET");
                self.close(remote_id);
                Ok(())
            }
            Message::Control(ControlMessage::Connect { local_id }) => {
                tracing::trace!(id.remote = %local_id, "process_inbound: CONNECT");
                todo!("eliza: implement registry...")
            }
        }
    }

    /// Start a locally-initiated connecting socket, returning the frame to send
    /// in order to initiate that connection.
    #[must_use]
    pub fn start_connecting(&mut self, bidi: SerBiDi) -> Option<ControlMessage> {
        let sock = Socket {
            state: State::Connecting,
            bidi,
        };
        let local_id = self.insert(sock)?;

        Some(ControlMessage::Connect { local_id })
    }

    /// Process an ack for a locally-initiated connecting socket with `local_id`.
    pub fn process_ack(&mut self, local_id: Id, remote_id: Id) -> Result<(), InboundError> {
        let Some(Entry::Occupied(ref mut sock)) = self.conns.get_mut(local_id) else {
            tracing::debug!(?local_id, ?remote_id, "process_ack: no such socket");
            return Err(InboundError::NoSocket);
        };

        match sock.state {
            State::Open {
                remote_id: real_remote_id,
            } => {
                tracing::trace!(
                    %local_id,
                    %remote_id,
                    %real_remote_id,
                    "process_ack: socket is not connecting"
                );
                Err(InboundError::AlreadyEstablished {
                    remote_id: real_remote_id,
                })
            }
            ref mut state @ State::Connecting => {
                *state = State::Open { remote_id };

                tracing::trace!(?local_id, ?remote_id, "process_ack: connection established");
                Ok(())
            }
        }
    }

    /// Accept a remote initiated connection with the provided `remote_id`.
    #[must_use]
    pub fn accept(&mut self, remote_id: Id, bidi: SerBiDi) -> ControlMessage {
        let sock = Socket {
            state: State::Open { remote_id },
            bidi,
        };

        match self.insert(sock) {
            // Accepted, we got a local ID!
            Some(local_id) => ControlMessage::Ack {
                remote_id,
                local_id,
            },
            // Conn table is full, can't accept this stream.
            None => ControlMessage::Nak { remote_id },
        }
    }

    /// Returns `true` if a connection with the provided ID was closed, `false` if
    /// no conn existed for that ID.
    pub fn close(&mut self, local_id: Id) -> bool {
        match self.conns.get_mut(local_id) {
            None => {
                tracing::trace!(?local_id, "close: ID greater than max conns ({CAPACITY})");
                false
            }
            Some(entry @ Entry::Occupied(_)) => {
                tracing::trace!(?local_id, self.len, "close: closing connection");
                *entry = Entry::Closed(self.next_id);
                self.next_id = local_id;
                self.len -= 1;
                true
            }
            Some(_) => {
                tracing::trace!(?local_id, "close: no connection for ID");
                false
            }
        }
    }

    pub fn bidi(&self, local_id: Id) -> Option<&SerBiDi> {
        self.conns.get(local_id).and_then(Entry::bidi)
    }

    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    fn insert(&mut self, socket: Socket) -> Option<Id> {
        // conn table full
        if self.len == CAPACITY {
            tracing::trace!(capacity = CAPACITY, "insert: conn table full");
            return None;
        }

        let local_id = self.next_id;

        self.next_id = match mem::replace(self.conns.get_mut(local_id)?, Entry::Occupied(socket)) {
            Entry::Unused => self.next_id.checked_add(1).expect("connection ID overflow"),
            Entry::Closed(next) => next,
            Entry::Occupied(_) => {
                unreachable!("we should never reassign to an occupied connection")
            }
        };

        self.len += 1;

        tracing::trace!(?local_id, self.len, "insert: added connection");

        Some(local_id)
    }
}

// === impl Id ===

impl Id {
    #[inline]
    #[must_use]
    fn to_index(self) -> usize {
        // subtract 1 because the link-control channel is at index 0
        self.0.get() as usize - 1
    }

    #[inline]
    fn checked_add(self, n: u16) -> Option<Self> {
        self.0.checked_add(n).map(Self)
    }
}

impl fmt::UpperHex for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, f)
    }
}

impl fmt::LowerHex for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:04x}")
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({self:04x})")
    }
}

// === impl LinkId ===

impl LinkId {
    pub const INTERFACE: Self = Self {
        local: None,
        remote: None,
    };

    pub(crate) fn invert(self) -> Self {
        Self {
            local: self.remote,
            remote: self.local,
        }
    }
}

impl fmt::UpperHex for LinkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { local, remote } = self;
        let local = local.map(|Id(x)| x.get()).unwrap_or(0);
        let remote = remote.map(|Id(x)| x.get()).unwrap_or(0);
        write!(f, "{remote:04X}:{local:04X}")
    }
}

impl fmt::LowerHex for LinkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { local, remote } = self;
        let local = local.map(|Id(x)| x.get()).unwrap_or(0);
        let remote = remote.map(|Id(x)| x.get()).unwrap_or(0);
        write!(f, "{remote:04x}:{local:04x}")
    }
}

impl fmt::Display for LinkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(self, f)
    }
}

// === impl Entries ===

impl<const CAPACITY: usize> Entries<CAPACITY> {
    #[inline]
    #[must_use]
    fn get(&self, local_id: Id) -> Option<&Entry> {
        self.0.get(local_id.to_index())
    }

    #[inline]
    #[must_use]
    fn get_mut(&mut self, local_id: Id) -> Option<&mut Entry> {
        self.0.get_mut(local_id.to_index())
    }
}

// === impl Entry ===

impl Entry {
    async fn reserve_send(&self) -> Result<SerPermit<'_>, InboundError> {
        self.bidi()
            .ok_or(InboundError::NoSocket)?
            .tx()
            .reserve()
            .await
            .map_err(|_| InboundError::ChannelClosed)
    }

    fn bidi(&self) -> Option<&SerBiDi> {
        match self {
            Entry::Occupied(ref sock) => Some(&sock.bidi),
            _ => None,
        }
    }
}

impl fmt::Display for InboundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSocket => f.write_str("no socket exists for this ID"),
            Self::AlreadyEstablished { remote_id } => write!(
                f,
                "cannot process ACK: a connection (remote ID {remote_id}) is already established"
            ),
            Self::ChannelClosed => f.write_str("local channel has closed"),
            Self::Deserialize(error) => write!(f, "failed to deserialize inbound message: {error}"),
        }
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use proptest::prelude::*;

//     proptest! {
//         #[test]
//         fn id_deserializes(local: Option<Id>, remote: Option<Id>) {
//             let data = b"hello world";
//             let id = dbg!(LinkId { local, remote });
//             let frame = dbg!(OutboundFrame { id, body: Body::Data(data) });
//             let bytes = postcard::to_allocvec(&frame).unwrap();
//             let actual_id = dbg!(postcard::from_bytes::<LinkId>(&bytes).unwrap());
//             prop_assert_eq!(id, actual_id);
//         }
//     }
// }
