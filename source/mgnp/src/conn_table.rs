use crate::{
    client::OutboundConnect,
    message::{Header, InboundFrame, OutboundFrame, Rejection, Reset},
    registry,
};
use core::{fmt, mem, num::NonZeroU16, task::Poll};
use tricky_pipe::{bidi::SerBiDi, mpsc::SerPermit, oneshot};

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
    Connecting(oneshot::Sender<Result<(), Rejection>>),
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
    channel: SerBiDi,
    reset: oneshot::Sender<Reset>,
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
    pub(crate) async fn next_outbound(&mut self) -> OutboundFrame<'_> {
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
                    channel,
                    ..
                }) = entry.socket()
                else {
                    continue;
                };

                // poll the socket's local channel to see if it has outbound
                // data or has closed.
                match channel.rx().poll_recv(cx) {
                    // a local data frame is ready to send!
                    Poll::Ready(Some(data)) => {
                        let local_id = Id::from_index(idx);
                        return Poll::Ready(OutboundFrame::data(*remote_id, local_id, data));
                    }

                    // the local stream has closed, so mark the socket as dead
                    // and generate a reset frame to tell the remote that it's
                    // closed.
                    Poll::Ready(None) => {
                        self.dead_index = Some(Id::from_index(idx));
                        return Poll::Ready(OutboundFrame::reset(
                            *remote_id,
                            Reset::BecauseISaidSo,
                        ));
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
            self.remove(local_id);
        }
    }

    /// Process an inbound frame.
    pub async fn process_inbound(
        &mut self,
        registry: &'_ impl registry::Registry,
        frame: InboundFrame<'_>,
    ) -> Option<OutboundFrame<'_>> {
        let span = tracing::trace_span!("process_inbound", id = %frame.header.link_id());
        let _enter = span.enter();
        match frame.header {
            // Inbound data frame from remote.
            Header::Data {
                local_id,
                remote_id,
            } => {
                tracing::trace!(
                    id.remote = %local_id,
                    id.local = %remote_id,
                    len = frame.body.len(),
                    "process_inbound: DATA",
                );
                // the remote peer's remote ID is our local ID.
                let id = remote_id;
                let Some(socket) = self.conns.get_mut(id) else {
                    tracing::debug!(
                        id.remote = %local_id,
                        id.local = %id,
                        "process_inbound(DATA): connection does not exist, resetting...",
                    );
                    return Some(OutboundFrame::reset(local_id, Reset::NoSuchConn));
                };

                // try to reserve send capacity on this socket.
                let reset = match socket.reserve_send().await {
                    Ok(permit) => match permit.send(frame.body) {
                        Ok(_) => return None,
                        Err(error) => {
                            // TODO(eliza): possibly it would be better if we
                            // just sent the deserialize error to the local peer
                            // and let it decide whether this should kill the
                            // connection or not? maybe by turning the server's
                            // client-to-server stream into `Result<ClientMsg,
                            // DecodeError>`s?
                            tracing::debug!(
                                id.remote = %local_id,
                                id.local = %id,
                                %error,
                                "process_inbound(DATA): failed to deserialize; resetting...",
                            );
                            Reset::bad_frame(error)
                        }
                    },
                    Err(reset) => reset,
                };
                tracing::trace!(
                    id.remote = %local_id,
                    id.local = %id,
                    reason = %reset,
                    "process_inbound(DATA): connection reset",
                );
                self.remove(id);
                Some(OutboundFrame::reset(local_id, reset))
            }
            Header::Ack {
                local_id,
                remote_id,
            } => {
                tracing::trace!(id.remote = %local_id, id.local = %remote_id, "process_inbound: ACK");
                self.process_ack(remote_id, local_id)
            }
            Header::Reject { remote_id, reason } => {
                tracing::trace!(id.local = %remote_id, ?reason, "process_inbound: REJECT");
                self.reject(remote_id, reason);
                None
            }
            Header::Reset { remote_id, reason } => {
                tracing::trace!(id.local = %remote_id, %reason, "process_inbound: RESET");
                let _closed = self.reset(remote_id, reason).await;
                tracing::trace!(id.local = %remote_id, closed = _closed, "process_inbound(RESET): connection closed");
                None
            }
            Header::Connect { local_id, identity } => {
                tracing::trace!(id.remote = %local_id, ?identity, "process_inbound: CONNECT");
                match registry.connect(identity, frame.body).await {
                    Ok(channel) => Some(self.accept(local_id, channel)),
                    Err(reason) => Some(OutboundFrame::nak(local_id, reason)),
                }
            }
        }
    }

    /// Start a locally-initiated connecting socket, returning the frame to send
    /// in order to initiate that connection.
    #[must_use]
    pub(crate) fn start_connecting(
        &mut self,
        connect: OutboundConnect,
    ) -> Option<OutboundFrame<'_>> {
        let OutboundConnect {
            hello,
            identity,
            channel,
            rsp,
            reset,
        } = connect;

        let local_id = match self.reserve_id() {
            Some(id) => id,
            None => {
                // if the local initiator dropped the response channel, that's
                // fine, we're bailing anyway!
                let _ = rsp.send(Err(Rejection::ConnTableFull(CAPACITY)));
                return None;
            }
        };

        self.insert_at(
            local_id,
            Socket {
                state: State::Connecting(rsp),
                channel,
                reset,
            },
        );

        Some(OutboundFrame::connect(local_id, identity, hello))
    }

    /// Process an ack for a locally-initiated connecting socket with `local_id`.
    fn process_ack(&mut self, local_id: Id, remote_id: Id) -> Option<OutboundFrame<'static>> {
        let Some(Entry::Occupied(ref mut sock)) = self.conns.get_mut(local_id) else {
            tracing::debug!(id.local = %local_id, id.remote = %remote_id, "process_ack: no such socket");
            return Some(OutboundFrame::reset(remote_id, Reset::NoSuchConn));
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
                Some(OutboundFrame::reset(remote_id, Reset::YesSuchConn))
            }
            ref mut state @ State::Connecting(_) => {
                let State::Connecting(rsp) = mem::replace(state, State::Open { remote_id }) else {
                    unreachable!(
                        "if this match arm matched, then the state should already be connecting!"
                    );
                };

                // tell the local connection initiator that they're okay
                if rsp.send(Ok(())).is_err() {
                    // local initiator is no longer there, reset!
                    tracing::debug!(
                        id.remote = %local_id,
                        id.local = %remote_id,
                        "process_ack: local initiator is no longer there; resetting"
                    );
                    return Some(OutboundFrame::reset(remote_id, Reset::BecauseISaidSo));
                }

                tracing::trace!(
                    id.remote = %local_id,
                    id.local = %remote_id,
                    "process_ack: connection established",
                );
                None
            }
        }
    }

    /// Accept a remote initiated connection with the provided `remote_id`.
    #[must_use]
    fn accept(&mut self, remote_id: Id, channel: SerBiDi) -> OutboundFrame<'_> {
        let sock = Socket {
            state: State::Open { remote_id },
            channel,
            ..
        };

        match self.insert(sock) {
            // Accepted, we got a local ID!
            Some(local_id) => OutboundFrame::ack(local_id, remote_id),
            // Conn table is full, can't accept this stream.
            None => OutboundFrame::nak(remote_id, Rejection::ConnTableFull(CAPACITY)),
        }
    }

    fn reject(&mut self, local_id: Id, rejection: Rejection) -> bool {
        match self.remove(local_id) {
            Some(Socket {
                state: State::Open { .. },
                ..
            }) => {
                tracing::warn!(
                    iid.local = %local_id,
                    ?rejection,
                    "reject: tried to REJECT an established connection. the remote *should* have sent a RESET instead",
                );
                false
            }
            Some(Socket {
                state: State::Connecting(rsp),
                ..
            }) => rsp.send(Err(rejection)).is_ok(),
            None => {
                tracing::warn!(
                    id.local = %local_id,
                    ?rejection,
                    "reject: tried to REJECT a non-existent connection",
                );
                false
            }
        }
    }

    async fn reset(&mut self, local_id: Id, reason: Reset) -> bool {
        tracing::trace!(id.local = %local_id, %reason, "reset: resetting connection...");
        let Some(Socket { state, reset, .. }) = self.remove(local_id) else {
            tracing::warn!(
                id.local = %local_id,
                %reason,
                "reset: tried to RESET a non-existent connection",
            );
            return false;
        };

        if matches!(state, State::Connecting(..)) {
            tracing::warn!(
                id.local = %local_id,
                %reason,
                "reset: tried to RESET an establishing connection. the remote *should* have sent a REJECT instead",
            );
            // TODO(eliza): send some kinda rejection?
            return false;
        }

        reset.send(reason).is_ok()
    }

    /// Returns `true` if a connection with the provided ID was closed, `false` if
    /// no conn existed for that ID.
    fn remove(&mut self, local_id: Id) -> Option<Socket> {
        match self.conns.get_mut(local_id) {
            None => {
                tracing::trace!(?local_id, "close: ID greater than max conns ({CAPACITY})");
                None
            }
            Some(entry @ Entry::Occupied(_)) => {
                tracing::trace!(?local_id, self.len, "close: closing connection");
                let Entry::Occupied(sock) = mem::replace(entry, Entry::Closed(self.next_id)) else {
                    unreachable!("what the fuck, we just matched this as an occupied entry!");
                };
                // if let Some(nak) = nak {
                //     match entry {
                //         Entry::Occupied(Socket {
                //             state: State::Connecting(rsp),
                //             ..
                //         }) => {
                //             let nacked = rsp.send(Err(nak)).is_ok();
                //             tracing::trace!(?local_id, ?nak, nacked, "close: sent nak");
                //         }
                //         Entry::Occupied(..) => {
                //
                //         }
                //         _ => unreachable!("we just matched an occupied entry!"),
                //     }
                // }

                self.next_id = local_id;
                self.len -= 1;
                Some(sock)
            }
            Some(_) => {
                tracing::trace!(?local_id, "close: no connection for ID");
                None
            }
        }
    }

    #[must_use]
    fn insert(&mut self, socket: Socket) -> Option<Id> {
        let local_id = self.reserve_id()?;
        self.insert_at(local_id, socket);
        Some(local_id)
    }

    #[must_use]
    fn reserve_id(&mut self) -> Option<Id> {
        // conn table full
        if self.len == CAPACITY {
            tracing::trace!(capacity = CAPACITY, "insert: conn table full");
            return None;
        }

        Some(self.next_id)
    }

    fn insert_at(&mut self, local_id: Id, socket: Socket) {
        let entry = self.conns.get_mut(local_id).expect("ID should be present");
        self.next_id = match mem::replace(entry, Entry::Occupied(socket)) {
            Entry::Unused => self.next_id.checked_add(1).expect("connection ID overflow"),
            Entry::Closed(next) => next,
            Entry::Occupied(_) => {
                unreachable!("we should never reassign to an occupied connection")
            }
        };

        self.len += 1;

        tracing::trace!(?local_id, self.len, "insert: added connection");
    }
}

// === impl Id ===

impl Id {
    // #[cfg(test)]
    // pub(crate) fn new(n: u16) -> Self {
    //     Self(NonZeroU16::new(n).expect("IDs must be non-zero"))
    // }

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
    async fn reserve_send(&self) -> Result<SerPermit<'_>, Reset> {
        self.channel()
            .ok_or(Reset::NoSuchConn)?
            .tx()
            .reserve()
            .await
            .map_err(|_| Reset::BecauseISaidSo)
    }

    fn socket(&self) -> Option<&Socket> {
        match self {
            Entry::Occupied(ref sock) => Some(sock),
            _ => None,
        }
    }

    fn channel(&self) -> Option<&SerBiDi> {
        self.socket().map(|sock| &sock.channel)
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
