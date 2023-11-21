//! Bidirectional channels.
//!
//! This module contains the [`BiDi`] and [`SerBiDi`] types, which combine a
//! [`Sender`] and [`Receiver`] or a [`DeserSender`] and [`SerReceiver`]
//! (respectively) into a single bidirectional channel which can both send and
//! receive messages to/from a remote peer.
use crate::mpsc::*;
use core::fmt;
use futures::FutureExt;

/// A bidirectional typed channel.
///
/// This channel consists of a [`Sender`] paired with a [`Receiver`], and can be
/// used to both send and receive typed messages to and from a remote peer.
#[must_use]
pub struct BiDi<In: 'static, Out: 'static, E: 'static> {
    tx: Sender<Out, E>,
    rx: Receiver<In, E>,
}

/// A bidirectional type-erased serializing channel.
///
/// This channel consists of a [`DeserSender`] paired with a [`SerReceiver`],
/// and can be  used to both send and receive serialized messages to and from a
/// remote peer.
#[must_use]
pub struct SerBiDi<E: 'static> {
    tx: DeserSender<E>,
    rx: SerReceiver<E>,
}

/// Events returned by [`BiDi::wait`] and [`SerBiDi::wait`].
#[derive(Debug)]
#[must_use]
pub enum Event<In, Out> {
    /// A message was received from the remote peer.
    Recv(In),
    /// The channel has capacity to send a message.
    SendReady(Out),
}

impl<In, Out, E> BiDi<In, Out, E>
where
    In: 'static,
    Out: 'static,
    E: Clone + 'static,
{
    /// Constructs a new `BiDi` from a [`Sender`] and a [`Receiver`].
    pub fn from_pair(tx: Sender<Out, E>, rx: Receiver<In, E>) -> Self {
        Self { tx, rx }
    }

    /// Consumes `self`, extracting the inner [`Sender`] and [`Receiver`].
    #[must_use]
    pub fn split(self) -> (Sender<Out, E>, Receiver<In, E>) {
        (self.tx, self.rx)
    }

    // /// Wait until the channel is either ready to send a message *or* a new
    // /// incoming message is received, whichever occurs first.
    // #[must_use]
    // pub async fn wait(&self) -> Option<Event<In, Permit<'_, Out, ()>>> {
    //     futures::select_biased! {
    //         res = self.tx.reserve().fuse() => {
    //             match res {
    //                 Ok(permit) => Some(Event::SendReady(permit)),
    //                 Err(_) => self.rx.recv().await.map(Event::Recv),
    //             }
    //         }
    //         recv = self.rx.recv().fuse() => {
    //             recv.map(Event::Recv)
    //         }
    //     }
    // }

    /// Borrows the **send half** of this bidirectional channel.
    ///
    /// This may be used to call methods such as [`Sender::send`], [`Sender::reserve`],
    /// [`Sender::try_reserve`], [`Sender::capacity`], et cetera, on the send
    /// half of the channel.
    #[must_use]
    pub fn tx(&self) -> &Sender<Out, E> {
        &self.tx
    }

    /// Borrows the **receive half** of this bidirectional channel.
    ///
    /// This may be used to call methods such as [`Receiver::recv`],
    /// [`Receiver::try_recv`], [`Receiver::capacity`], et cetera, on the
    /// receive half of the channel.
    #[must_use]
    pub fn rx(&self) -> &Receiver<In, E> {
        &self.rx
    }

    /// Returns `true` if **both halves** of this bidirectional channel are
    /// empty.
    ///
    /// This method returns `true` if and only if *both the send and receive
    /// halves* of this channel are empty. To check if only one the send or
    /// receive half is empty, use
    /// [`self.tx()`]`.`[`is_empty()`](Sender::is_empty) or
    /// [`self.rx()`]`.`[`is_empty()`](Receiver::is_empty), respectively.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tx.is_empty() && self.rx.is_empty()
    }

    /// Returns `true` if **both halves** of this bidirectional channel are
    /// full.
    ///
    /// This method returns `true` if and only if *both the send and receive
    /// halves* of this channel are full. To check if only one the send or
    /// receive half is full, use
    /// [`self.tx()`]`.`[`is_full()`](Sender::is_full) or
    /// [`self.rx()`]`.`[`is_full()`](Receiver::is_full), respectively.
    #[inline]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.tx.is_full() && self.rx.is_full()
    }
}

impl<In, Out, E> fmt::Debug for BiDi<In, Out, E>
where
    In: 'static,
    Out: 'static,
    E: 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { tx, rx } = self;
        f.debug_struct("BiDi")
            .field("tx", tx)
            .field("rx", rx)
            .finish()
    }
}

// === impl SerBiDi ===

impl<E: Clone + 'static> SerBiDi<E> {
    /// Constructs a new `SerBiDi` from a [`DeserSender`] and a [`SerReceiver`].
    pub fn from_pair(tx: DeserSender<E>, rx: SerReceiver<E>) -> Self {
        Self { tx, rx }
    }

    /// Consumes `self`, extracting the inner [`DeserSender`] and [`SerReceiver`].
    #[must_use]
    pub fn split(self) -> (DeserSender<E>, SerReceiver<E>) {
        (self.tx, self.rx)
    }

    // /// Wait until the channel is either ready to send a message *or* a new
    // /// incoming message is received, whichever occurs first.
    // #[must_use]
    // pub async fn wait(&self) -> Option<Event<SerRecvRef<'_>, SerPermit<'_, ()>>> {
    //     futures::select_biased! {
    //         res = self.tx.reserve().fuse() => {
    //             match res {
    //                 Ok(permit) => Some(Event::SendReady(permit)),
    //                 Err(_) => self.rx.recv().await.map(Event::Recv),
    //             }
    //         }
    //         recv = self.rx.recv().fuse() => {
    //             recv.map(Event::Recv)
    //         }
    //     }
    // }

    /// Borrows the **send half** of this bidirectional channel.
    ///
    /// This may be used to call methods such as [`DeserSender::reserve`],
    /// [`DeserSender::try_reserve`], [`DeserSender::capacity`], et cetera, on
    /// the send half of the channel.
    #[must_use]
    pub fn tx(&self) -> &DeserSender<E> {
        &self.tx
    }

    /// Borrows the **receive half** of this bidirectional channel.
    ///
    /// This may be used to call methods such as [`SerReceiver::recv`],
    /// [`SerReceiver::try_recv`], [`SerReceiver::capacity`], et cetera, on the
    /// receive half of the channel.
    #[must_use]
    pub fn rx(&self) -> &SerReceiver<E> {
        &self.rx
    }

    /// Returns `true` if **both halves** of this bidirectional channel are
    /// empty.
    ///
    /// This method returns `true` if and only if *both the send and receive
    /// halves* of this channel are empty. To check if only one the send or
    /// receive half is empty, use
    /// [`self.tx()`]`.`[`is_empty()`](DeserSender::is_empty) or
    /// [`self.rx()`]`.`[`is_empty()`](SerReceiver::is_empty), respectively.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tx.is_empty() && self.rx.is_empty()
    }

    /// Returns `true` if **both halves** of this bidirectional channel are
    /// full.
    ///
    /// This method returns `true` if and only if *both the send and receive
    /// halves* of this channel are full. To check if only one the send or
    /// receive half is full, use
    /// [`self.tx()`]`.`[`is_full()`](DeserSender::is_full) or
    /// [`self.rx()`]`.`[`is_full()`](SerReceiver::is_full), respectively.
    #[inline]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.tx.is_full() && self.rx.is_full()
    }
}

impl<E> fmt::Debug for SerBiDi<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { tx, rx } = self;
        f.debug_struct("SerBiDi")
            .field("tx", tx)
            .field("rx", rx)
            .finish()
    }
}
