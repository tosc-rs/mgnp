use crate::{Frame, Wire};
use core::{mem, num::NonZeroU16};

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(NonZeroU16);

pub struct ConnTable<T, const CAPACITY: usize> {
    conns: Entries<T, CAPACITY>,
    next_id: Id,
    len: usize,
}

#[derive(Debug)]
pub enum State {
    Open { remote_id: Id },
    Connecting,
}

#[derive(Debug)]
pub enum OutboundFrame<F> {
    Connect { local_id: Id },
    Ack { local_id: Id, remote_id: Id },
    Nak { remote_id: Id },
    Data { local_id: Id, data: F },
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ConfirmError {
    /// The connection tracking table doesn't have a connection for the provided ID.
    NoSocket,
    /// The connection was already acked by the remote, so they acked it again
    /// for some reason?
    AlreadyEstablished { remote_id: Id },
}

#[derive(Debug)]
#[non_exhaustive]
pub enum InboundError {
    /// The connection tracking table doesn't have a connection for the provided ID.
    NoSocket,
}

#[derive(Debug)]
struct Socket<T> {
    state: State,
    bidi: T,
}

enum Entry<T> {
    Unused,
    Closed(Id),
    Occupied(Socket<T>),
}

/// Wrapper struct so we can have a `get_mut` that's indexed by `Id`, basically.
struct Entries<T, const CAPACITY: usize>([Entry<T>; CAPACITY]);

impl<T, const CAPACITY: usize> ConnTable<T, CAPACITY>
where
    // XXX(eliza): we are using "wire" for in-mem bidis here. maybe the trait
    // should be called something else?
    T: Wire,
{
    const ENTRY_UNUSED: Entry<T> = Entry::Unused;

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
    pub async fn next_outbound(&mut self) -> OutboundFrame<T::Frame> {
        todo!()
    }

    /// Process an inbound frame.
    pub async fn process_inbound(&mut self, frame: T::Frame) -> Result<(), InboundError> {
        todo!("eliza")
    }

    /// Start a locally-initiated connecting socket, returning the frame to send
    /// in order to initiate that connection.
    #[must_use]
    pub fn start_connecting(&mut self, bidi: T) -> Option<OutboundFrame<T::Frame>> {
        let sock = Socket {
            state: State::Connecting,
            bidi,
        };
        let local_id = self.insert(sock)?;

        Some(OutboundFrame::Connect { local_id })
    }

    /// Process an ack for a locally-initiated connecting socket with `local_id`.
    pub fn confirm(&mut self, local_id: Id, remote_id: Id) -> Result<(), ConfirmError> {
        let Some(Entry::Occupied(ref mut sock)) = self.conns.get_mut(local_id) else {
            tracing::trace!(?local_id, ?remote_id, "process_ack: no such socket");
            return Err(ConfirmError::NoSocket);
        };

        match sock.state {
            State::Open {
                remote_id: real_remote_id,
            } => {
                tracing::trace!(
                    ?local_id,
                    ?remote_id,
                    ?real_remote_id,
                    "process_ack: socket is not connecting"
                );
                Err(ConfirmError::AlreadyEstablished {
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
    pub fn accept(&mut self, remote_id: Id, bidi: T) -> OutboundFrame<T::Frame> {
        let sock = Socket {
            state: State::Open { remote_id },
            bidi,
        };

        match self.insert(sock) {
            // Accepted, we got a local ID!
            Some(local_id) => OutboundFrame::Ack {
                local_id,
                remote_id,
            },
            // Conn table is full, can't accept this stream.
            None => OutboundFrame::Nak { remote_id },
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

    pub fn get(&self, local_id: Id) -> Option<&T> {
        self.conns.get(local_id).and_then(Entry::bidi)
    }

    pub fn get_mut(&mut self, local_id: Id) -> Option<&mut T> {
        self.conns.get_mut(local_id).and_then(Entry::bidi_mut)
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
    fn insert(&mut self, socket: Socket<T>) -> Option<Id> {
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

// === impl Entries ===

impl<T, const CAPACITY: usize> Entries<T, CAPACITY> {
    #[inline]
    #[must_use]
    fn get(&self, local_id: Id) -> Option<&Entry<T>> {
        self.0.get(local_id.to_index())
    }

    #[inline]
    #[must_use]
    fn get_mut(&mut self, local_id: Id) -> Option<&mut Entry<T>> {
        self.0.get_mut(local_id.to_index())
    }
}

// === impl Entries ===

impl<T> Entry<T> {
    fn bidi(&self) -> Option<&T> {
        match self {
            Entry::Occupied(ref sock) => Some(&sock.bidi),
            _ => None,
        }
    }

    fn bidi_mut(&mut self) -> Option<&mut T> {
        match self {
            Entry::Occupied(ref mut sock) => Some(&mut sock.bidi),
            _ => None,
        }
    }
}
