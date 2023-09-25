#![cfg_attr(not(test), no_std)]
use core::mem;

// #[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
// pub struct Id(NonZeroUsize);

// TODO(eliza): should these be u32 or u64?
pub type Id = usize;

pub struct ConnTable<const CAPACITY: usize> {
    conns: [Entry; CAPACITY],
    next_id: Id,
    len: usize,
}

enum Entry {
    Unused,
    Closed(Id),
    Occupied(Socket),
}

#[derive(Debug)]
pub struct Socket {
    pub state: State,
}

#[derive(Debug)]
pub enum State {
    Open { remote_id: Id },
    Connecting,
}

#[derive(Debug)]
pub enum Frame {
    Connect { local_id: Id },
    Ack { local_id: Id, remote_id: Id },
    Nak { remote_id: Id },
}

/// Indicates that the `ConnTable` was full, returning the inserted item.
pub struct Full<T>(T);

impl<const CAPACITY: usize> ConnTable<CAPACITY> {
    const ENTRY_UNUSED: Entry = Entry::Unused;

    #[must_use]
    pub const fn new() -> Self {
        Self {
            conns: [Self::ENTRY_UNUSED; CAPACITY],
            // ID 0 is the link-control channel
            next_id: 1,
            len: 0,
        }
    }

    #[must_use]
    pub fn connect(&mut self) -> Option<Frame> {
        let sock = Socket {
            state: State::Connecting,
        };
        let local_id = self.insert(sock)?;

        Some(Frame::Connect { local_id })
    }

    #[must_use]
    pub fn accept(&mut self, remote_id: Id) -> Frame {
        let sock = Socket {
            state: State::Open { remote_id },
        };

        match self.insert(sock) {
            // Accepted, we got a local ID!
            Some(local_id) => Frame::Ack {
                local_id,
                remote_id,
            },
            // Conn table is full, can't accept this stream.
            None => Frame::Nak { remote_id },
        }
    }

    /// Returns `true` if a connection with the provided ID was closed, `false` if
    /// no conn existed for that ID.
    pub fn close(&mut self, local_id: Id) -> bool {
        match self.conns.get_mut(local_id - 1) {
            None => {
                tracing::trace!(local_id, "close: ID greater than max conns ({CAPACITY})");
                false
            }
            Some(entry @ Entry::Occupied(_)) => {
                tracing::trace!(local_id, "close: closing connection");
                *entry = Entry::Closed(self.next_id);
                self.next_id = local_id;
                self.len -= 1;
                true
            }
            Some(_) => {
                tracing::trace!(local_id, "close: no connection for ID");
                false
            }
        }
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
            return None;
        }

        let id = self.next_id;

        self.next_id = match mem::replace(self.conns.get_mut(id - 1)?, Entry::Occupied(socket)) {
            Entry::Unused => self.next_id.checked_add(1).expect("connection ID overflow"),
            Entry::Closed(next) => next,
            Entry::Occupied(_) => {
                unreachable!("we should never reassign to an occupied connection")
            }
        };

        self.len += 1;

        Some(id)
    }
}
