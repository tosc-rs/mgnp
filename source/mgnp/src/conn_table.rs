use crate::{
    message::{ControlMessage, InboundMessage, Nak, OutboundMessage},
    registry,
};
use core::{fmt, mem, num::NonZeroU16, task::Poll};
use tricky_pipe::{bidi::SerBiDi, mpsc::SerPermit};

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

pub(crate) struct ConnTable<const CAPACITY: usize> {
    conns: Entries<CAPACITY>,
    next_id: Id,
    len: usize,
    dead_index: Option<Id>,
}

#[derive(Debug)]
enum State {
    Open { remote_id: Id },
    Connecting,
}

#[derive(Debug)]
#[non_exhaustive]
enum InboundError {
    /// The connection tracking table doesn't have a connection for the provided ID.
    NoSocket,

    /// The local channel for this socket has closed.
    ChannelClosed,
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
    pub(crate) const fn new() -> Self {
        Self {
            conns: Entries([Self::ENTRY_UNUSED; CAPACITY]),
            // ID 0 is the link-control channel
            next_id: Id(unsafe { NonZeroU16::new_unchecked(1) }),
            len: 0,
            dead_index: None,
        }
    }

    /// Returns the next outbound frame.
    pub(crate) async fn next_outbound(&mut self) -> OutboundMessage<'_> {
        // remove any dead socket that closed during the previous `next_outbound` call.
        self.cleanup_dead();

        futures::future::poll_fn(|cx| {
            // TODO(eliza): would be nice if we didn't loop over all sockets
            // after a wakeup, and could instead use the `Waker` index to more
            // intelligently track which socket woke us, or something. But
            // that's Hard...
            for (idx, entry) in self.conns.0.iter().enumerate() {
                // Skip entries that are unoccupied or whose sockets are waiting
                // on the remote to initiate a connection.
                let Some(Socket {
                    state: State::Open { remote_id },
                    bidi,
                }) = entry.socket()
                else {
                    continue;
                };

                // poll the socket's local channel to see if it has outbound
                // data or has closed.
                match bidi.rx().poll_recv(cx) {
                    // a local data frame is ready to send!
                    Poll::Ready(Some(data)) => {
                        let local_id = Id::from_index(idx);
                        return Poll::Ready(OutboundMessage::Data {
                            local_id,
                            remote_id: *remote_id,
                            data,
                        });
                    }

                    // the local stream has closed, so mark the socket as dead
                    // and generate a reset frame to tell the remote that it's
                    // closed.
                    Poll::Ready(None) => {
                        self.dead_index = Some(Id::from_index(idx));
                        return Poll::Ready(OutboundMessage::reset(*remote_id));
                    }

                    // nothing to do, move on to the next socket.
                    Poll::Pending => {}
                }
            }

            // if no sockets are ready, yield until one is.
            Poll::Pending
        })
        .await
    }

    fn cleanup_dead(&mut self) {
        // receiving a data frame from the conn table borrows it, so we must
        // remove the dead index from the *previous* next_outbound call before
        // we borrow the whole conn table to poll it. yes, this is confusing and
        // weird...
        if let Some(local_id) = self.dead_index.take() {
            tracing::debug!(id.local = %local_id, "removing closed stream from dead index");
            self.close(local_id);
        }
    }

    /// Process an inbound frame.
    pub async fn process_inbound(
        &mut self,
        registry: &'_ impl registry::Registry,
        msg: InboundMessage<'_>,
    ) -> Option<OutboundMessage<'_>> {
        let span = tracing::trace_span!("process_inbound", id = %msg.link_id());
        let _enter = span.enter();
        match msg {
            // Inbound data frame from remote.
            InboundMessage::Data {
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
                let Some(socket) = self.conns.get_mut(id) else {
                    tracing::debug!("process_inbound: no socket for data frame, resetting...");
                    return Some(OutboundMessage::reset(local_id));
                };

                // try to reserve send capacity on this socket.
                let error = match socket.reserve_send().await {
                    Ok(permit) => match permit.send(data) {
                        Ok(_) => return None,
                        Err(error) => {
                            tracing::debug!(%error, "process_inbound: failed to deserialize data");
                            // TODO(eliza): we should probably tell the peer
                            // that they sent us something bad...
                            return None;
                        }
                    },
                    Err(error) => error,
                };

                // otherwise, we couldn't reserve a send permit because the
                // channel has closed locally.
                tracing::trace!("process_inbound: local error: {error}; resetting...");
                self.close(id);
                Some(OutboundMessage::reset(local_id))
            }
            InboundMessage::Control(ControlMessage::Ack {
                local_id,
                remote_id,
            }) => {
                tracing::trace!(id.remote = %local_id, id.local = %remote_id, "process_inbound: ACK");
                self.process_ack(remote_id, local_id)
            }
            InboundMessage::Control(ControlMessage::Nak { remote_id, reason }) => {
                tracing::trace!(id.local = %remote_id, ?reason, "process_inbound: NAK");
                // TODO(eliza): send error to the initiator
                self.close(remote_id);
                None
            }
            InboundMessage::Control(ControlMessage::Reset { remote_id }) => {
                tracing::trace!(id.local = %remote_id, "process_inbound: RESET");
                let _closed = self.close(remote_id);
                tracing::trace!(id.local = %remote_id, closed = _closed, "process_inbound: RESET ->");
                None
            }
            InboundMessage::Control(ControlMessage::Connect {
                local_id,
                identity,
                hello,
            }) => {
                tracing::trace!(id.remote = %local_id, ?identity, "process_inbound: CONNECT");
                match registry.connect(identity, hello).await {
                    Ok(bidi) => {
                        let rsp = self.accept(local_id, bidi);
                        Some(OutboundMessage::Control(rsp))
                    }
                    Err(reason) => Some(OutboundMessage::Control(ControlMessage::Nak {
                        remote_id: local_id,
                        reason,
                    })),
                }
            }
        }
    }

    /// Start a locally-initiated connecting socket, returning the frame to send
    /// in order to initiate that connection.
    #[must_use]
    pub(crate) fn start_connecting<'data>(
        &mut self,
        identity: registry::Identity,
        hello: &'data [u8],
        bidi: SerBiDi,
    ) -> Option<ControlMessage<'data>> {
        let sock = Socket {
            state: State::Connecting,
            bidi,
        };
        let local_id = self.insert(sock)?;

        Some(ControlMessage::Connect {
            local_id,
            hello,
            identity,
        })
    }

    /// Process an ack for a locally-initiated connecting socket with `local_id`.
    fn process_ack(&mut self, local_id: Id, remote_id: Id) -> Option<OutboundMessage<'_>> {
        let Some(Entry::Occupied(ref mut sock)) = self.conns.get_mut(local_id) else {
            tracing::debug!(id.local = %local_id, id.remote = %remote_id, "process_ack: no such socket");
            return Some(OutboundMessage::reset(remote_id));
        };

        match sock.state {
            State::Open {
                remote_id: real_remote_id,
            } => {
                // this socket is already open locally!
                tracing::debug!(
                    id.local = %local_id,
                    id.remote = %remote_id,
                    id.actual_remote = %real_remote_id,
                    "process_ack: socket is not connecting"
                );
                Some(OutboundMessage::reset(remote_id))
            }
            ref mut state @ State::Connecting => {
                *state = State::Open { remote_id };

                tracing::trace!(?local_id, ?remote_id, "process_ack: connection established");
                None
            }
        }
    }

    /// Accept a remote initiated connection with the provided `remote_id`.
    #[must_use]
    fn accept(&mut self, remote_id: Id, bidi: SerBiDi) -> ControlMessage {
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
            None => ControlMessage::Nak {
                remote_id,
                reason: Nak::ConnTableFull(CAPACITY),
            },
        }
    }

    /// Returns `true` if a connection with the provided ID was closed, `false` if
    /// no conn existed for that ID.
    fn close(&mut self, local_id: Id) -> bool {
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
    #[cfg(not(debug_assertions))]
    #[must_use]
    fn from_index(idx: usize) -> Self {
        let id = (idx as u16).saturating_add(1);
        Self(unsafe { NonZeroU16::new_unchecked(id) })
    }

    #[cfg(debug_assertions)]
    #[must_use]
    fn from_index(idx: usize) -> Self {
        let id = match u16::try_from(idx) {
            Ok(x) => x,
            Err(_) => {
                unreachable!("conn table indices may not exceed u16::MAX; tried to convert {idx}")
            }
        };
        let id = NonZeroU16::new(id.saturating_add(1))
            .expect("we just added 1 to the index, so it must be greater than 0");
        Self(id)
    }

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
    // #[inline]
    // // #[must_use]
    // // fn get(&self, local_id: Id) -> Option<&Entry> {
    // //     self.0.get(local_id.to_index())
    // // }

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

    fn socket(&self) -> Option<&Socket> {
        match self {
            Entry::Occupied(ref sock) => Some(sock),
            _ => None,
        }
    }

    fn bidi(&self) -> Option<&SerBiDi> {
        self.socket().map(|sock| &sock.bidi)
    }
}

impl fmt::Display for InboundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSocket => f.write_str("no socket exists for this ID"),
            Self::ChannelClosed => f.write_str("local channel has closed"),
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
