#![warn(missing_docs)]
use crate::loom::cell::{self, UnsafeCell};
use core::{
    fmt,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    ptr,
};
use serde::{de::DeserializeOwned, Serialize};

#[cfg(not(test))]
macro_rules! test_dbg {
    ($x:expr) => {
        $x
    };
}

#[cfg(test)]
macro_rules! test_dbg {
    ($x:expr) => {
        match $x {
            x => {
                tracing::debug!("{} = {x:?}", stringify!($x));
                x
            }
        }
    };
}

#[cfg(not(test))]
macro_rules! test_println {
    ($($arg:tt)*) => {};
}

#[cfg(test)]
macro_rules! test_println {
    ($($arg:tt)*) => {
        tracing::debug!($($arg)*);
    };
}

#[cfg(not(test))]
macro_rules! test_span {
    ($($arg:tt)*) => {};
}

#[cfg(test)]
macro_rules! test_span {
    ($($arg:tt)*) => {
        let _span = tracing::debug_span!($($arg)*).entered();
    };
}

#[cfg(any(test, feature = "alloc"))]
mod arc_impl;
mod channel_core;
pub mod error;
mod static_impl;

use self::{
    channel_core::{DeserVtable, ErasedPipe, ErasedSlice, Reservation, SerVtable, TypedPipe},
    error::*,
};

pub use self::static_impl::*;

#[cfg(any(test, feature = "alloc"))]
pub use self::arc_impl::*;

/// Receives `T`-typed values from associated [`Sender`]s or [`SerSender`]s.
///
/// A `Receiver` for a channel can be obtained using the
/// [`StaticTrickyPipe::receiver`] and [`TrickyPipe::receiver`] methods.
pub struct Receiver<T: 'static> {
    pipe: TypedPipe<T>,
}

/// Sends `T`-typed values to an associated [`Receiver`]s or [`SerReceiver`].
///
/// A `Sender` for a channel can be obtained using the
/// [`StaticTrickyPipe::sender`] and [`TrickyPipe::sender`] methods.
pub struct Sender<T: 'static> {
    pipe: TypedPipe<T>,
}

/// Receives serialized values from associated [`Sender`]s or [`SerSender`]s.
///
/// A `SerReceiver` for a channel can be obtained using the
/// [`StaticTrickyPipe::ser_receiver`] and [`TrickyPipe::ser_receiver`] methods,
/// when the channel's message type implements [`Serialize`]. Messages may be
/// sent as typed values by a [`Sender`], or as serialized bytes by a [`SerSender`].
pub struct SerReceiver {
    pipe: ErasedPipe,
    vtable: &'static SerVtable,
}

/// Sends serialized values to an associated [`Receiver`] or [`SerReceiver`].
///
/// A `SerSender` for a channel can be obtained using the
/// [`StaticTrickyPipe::ser_sender`] or [`TrickyPipe::ser_sender`] methods,
/// when the channel's message type implements [`DeserializeOwned`]. Messages may be
/// received as deserialized typed values by a [`Receiver`], or as serialized
/// bytes by a [`SerReceiver`].
pub struct SerSender {
    pipe: ErasedPipe,
    vtable: &'static DeserVtable,
}

/// A reference to a type-erased, serializable message received from a
/// [`SerReceiver`].
///
/// This type is returned by the [`SerReceiver::try_recv`] and
/// [`SerReceiver::recv`] methods.
///
/// The message may be serialized to a `&mut [u8]` using the [`to_slice`] or
/// [`to_slice_framed`] methods. If the "alloc" feature flag is enabled, the
/// message may also be serialized to an owned [`Vec`]`<u8>` using the
/// [`to_vec`] or [`to_vec_framed`] methods.
///
/// [`to_slice`]: Self::to_slice
/// [`to_slice_framed`]: Self::to_slice_framed
/// [`Vec`]: alloc::vec::Vec
/// [`to_vec`]: Self::to_vec
/// [`to_vec_framed`]: Self::to_vec_framed
#[must_use = "a `SerRecvRef` does nothing unless the `to_slice`, \
    `to_slice_framed`, `to_vec`, or `to_vec_framed` methods are called"]
pub struct SerRecvRef<'pipe> {
    res: Reservation<'pipe>,
    elems: ErasedSlice,
    vtable: &'static SerVtable,
}

/// A permit to send a single `T`-typed value to a channel.
///
/// This type is returned by the [`Sender::try_reserve`] and [`Sender::reserve`]
/// methods.
///
/// To send a `T`-typed value, call the [`send`] method on this type, consuming
/// the `Permit`. Dropping the `Permit` without sending a value will release
/// the reserved channel capacity to be used by other senders.
///
/// Alternatively, this type implements [`DerefMut`]`<Target =
/// `[`MaybeUninit`]`<T>>`, which may be used to write directly to the reserved
/// slot in the channel's buffer. This may improve performance in some cases.
/// When using the [`DerefMut`] implementation to write a message to the
/// channel's buffer, the [`commit`] message must be called to complete sending
/// the message. Dropping the `Permit` without calling [`commit`] will release
/// the reserved channel capacity without sending a value.
///
/// [`send`]: Self::send
/// [`commit`]: Self::commit
#[must_use = "a `Permit` does nothing unless the `send` or `commit` \
              methods are called"]
pub struct Permit<'core, T> {
    // load bearing drop ordering lol lmao
    cell: cell::MutPtr<MaybeUninit<T>>,
    pipe: Reservation<'core>,
}

/// A permit to send a single serialized value to a channel.
///
/// This type is returned by the [`SerSender::try_reserve`] and
/// [`SerSender::reserve`] methods.
///
/// To send a serialized value, call the [`send`] method on this type. If the
/// serialized bytes are a COBS frame call the [`send_framed`] method instead.
/// Sending a value consumes the `SerPermit`  Dropping the `SerPermit` without
/// sending a value will release the reserved channel capacity to be used by
/// other senders.
///
/// [`send`]: Self::send
/// [`send_framed`]: Self::send_framed
#[must_use = "a `SerPermit` does nothing unless the `send` or `send_framed` '
              methods are called"]
pub struct SerPermit<'core> {
    res: Reservation<'core>,
    elems: ErasedSlice,
    vtable: &'static DeserVtable,
}

type Cell<T> = UnsafeCell<MaybeUninit<T>>;

// === impl Receiver ===

impl<T> Receiver<T> {
    /// Attempts to receive the next message from the channel, without waiting
    /// for a new message to be sent.
    ///
    /// If there are currently messages in the channel's queue, this method
    /// returns the next message. Otherwise, if the channel has been closed, or
    /// if no messages are currently available, this method returns a
    /// [`TryRecvError`].
    ///
    /// To wait for a new message to be sent, rather than returning an error,
    /// use the [`recv`](Self::recv) method, instead.
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(T)` if a message was received from the channel.
    /// - [`Err`]`(`[`TryRecvError::Closed`]``)` if the channel has been closed
    ///   (all [`Sender`]s and [`SerSender`]s have been dropped) *and* all
    ///   messages sent before the channel closed have already been received.
    /// - [`Err`]`(`[`TryRecvError::Empty`]`)` if there are currently no
    ///   messages in the queue, but the channel has not been closed.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.pipe
            .core()
            .try_dequeue()
            .map(|res| self.take_value(res))
    }

    /// Receives the next message from the channel.
    ///
    /// This method returns [`None`] if the channel has been closed (all
    /// [`Sender`]s and [`SerSender`]s have been dropped) *and* all messages
    /// sent before the channel closed have been received.
    ///
    /// If the channel has not yet been closed, but there are no messages
    /// currently available in the queue, this method yields and waits for a new
    /// message to be sent, or for the channel to close.
    ///
    /// To return an error rather than waiting, use the
    /// [`try_recv`](Self::try_recv) method, instead.
    ///
    /// # Cancellation Safety
    ///
    /// This method is cancel-safe. If `recv` is used as part of a `select!` or
    /// other mechanism for waiting for the first of multiple futures to
    /// complete, and another future completes first, it is guaranteed that no
    /// message will be received from the channel.
    pub async fn recv(&self) -> Option<T> {
        self.pipe
            .core()
            .dequeue()
            .await
            .map(|res| self.take_value(res))
    }

    #[inline(always)]
    fn take_value(&self, res: Reservation<'_>) -> T {
        self.pipe.elems()[res.idx as usize].with(|ptr| unsafe { (*ptr).assume_init_read() })
    }

    /// Returns `true` if this channel is empty.
    ///
    /// If this method returns `true`, calling [`Receiver::recv`] or
    /// [`SerReceiver::try_recv`] will yield until a new message is sent to the
    /// channel. Any calls to [`Receiver::try_recv`] or
    /// [`SerReceiver::try_recv`] while the channel is empty will return
    /// [`TryRecvError::Empty`].
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pipe.core().is_empty()
    }

    /// Returns `true` if this channel is full.
    ///
    /// If this method returns `true`, then any calls to [`Sender::reserve`] or
    /// [`SerSender::reserve`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_reserve`] or [`SerSender::try_reserve`] will return an
    /// error.
    #[inline]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.pipe.core().is_full()
    }

    /// Returns the number of messages currently in the channel.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.pipe.core().len()
    }

    /// Returns the **maximum capacity** of the channel.
    ///
    /// This is the maximum number of messages that may be queued before senders
    /// must wait for additional capacity to become available. The capacity of
    /// the channel is determined *when it is constructed*, and the value
    /// returned by this method will never change over the channel's lifetime,
    /// regardless of the current [length](Self::len) of the channel.
    ///
    /// To determine the current remaining capacity in the channel, use the
    /// [`remaining`](Self::remaining) method, instead.
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.pipe.core().capacity as usize
    }

    /// Returns the **current remaining capacity** of this channel.
    ///
    /// This is equivalent to subtracting [`self.len()`](Self::len) from
    /// [`self.capacity()`](Self::capacity).
    #[inline]
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.len() - self.capacity()
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.pipe.core().close_rx();
    }
}

// === impl SerReceiver ===

impl SerReceiver {
    /// Attempts to receive the serialized representation of next message from
    /// the channel, without waiting for a new message to be sent if none are
    /// available.
    ///
    /// If there are currently messages in the channel's queue, this method
    /// returns the next message. Otherwise, if the channel has been closed, or
    /// if no messages are currently available, this method returns a
    /// [`TryRecvError`].
    ///
    /// To wait for a new message to be sent, rather than returning an error,
    /// use the [`recv`](Self::recv) method, instead.
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`SerRecvRef`]`)` if a message was received from the
    ///   channel. The [`SerRecvRef::to_slice`] and [`SerRecvRef::to_vec`]
    ///   methods can be used to serialize the binary representation of the
    ///   message to a `&mut [u8]` or to a [`Vec`]`<u8>`, respectively. To
    ///   serialize the message as a COBS frame, use the
    ///   [`SerRecvRef::to_slice_framed`] or [`SerRecvRef::to_vec_framed`]
    ///   methods, instead.
    /// - [`Err`]`(`[`TryRecvError::Closed`]``)` if the channel has been closed
    ///   (all [`Sender`]s and [`SerSender`]s have been dropped) *and* all
    ///   messages sent before the channel closed have already been received.
    /// - [`Err`]`(`[`TryRecvError::Empty`]`)` if there are currently no
    ///   messages in the queue, but the channel has not been closed.
    ///
    /// [`Vec`]: alloc::vec::Vec
    pub fn try_recv(&self) -> Result<SerRecvRef<'_>, TryRecvError> {
        let res = self.pipe.core().try_dequeue()?;
        Ok(SerRecvRef {
            res,
            elems: self.pipe.elems(),
            vtable: self.vtable,
        })
    }

    /// Receives the next message from the channel, returning a [`SerRecvRef`]
    /// that can be used to serialize that message as bytes.
    ///
    /// This method returns [`None`] if the channel has been closed (all
    /// [`Sender`]s and [`SerSender`]s have been dropped) *and* all messages
    /// sent before the channel closed have been received.
    ///
    /// If the channel has not yet been closed, but there are no messages
    /// currently available in the queue, this method yields and waits for a new
    /// message to be sent, or for the channel to close.
    ///
    /// To return an error rather than waiting, use the
    /// [`try_recv`](Self::try_recv) method, instead.
    ///
    /// # Returns
    ///
    /// - [`Some`]`(`[`SerRecvRef`]`)` if a message was received from the
    ///   channel. The [`SerRecvRef::to_slice`] and [`SerRecvRef::to_vec`]
    ///   methods can be used to serialize the binary representation of the
    ///   message to a `&mut [u8]` or to a [`Vec`]`<u8>`, respectively. To
    ///   serialize the message as a COBS frame, use the
    ///   [`SerRecvRef::to_slice_framed`] or [`SerRecvRef::to_vec_framed`]
    ///   methods, instead.
    /// - [`None`] if the channel is closed *and* all messages have been
    ///   received.
    ///
    /// # Cancellation Safety
    ///
    /// This method is cancel-safe. If `recv` is used as part of a `select!` or
    /// other mechanism for waiting for the first of multiple futures to
    /// complete, and another future completes first, it is guaranteed that no
    /// message will be received from the channel.
    ///
    /// [`Vec`]: alloc::vec::Vec
    pub async fn recv(&self) -> Option<SerRecvRef<'_>> {
        self.pipe.core().dequeue().await.map(|res| SerRecvRef {
            res,
            elems: self.pipe.elems(),
            vtable: self.vtable,
        })
    }

    /// Returns `true` if this channel is empty.
    ///
    /// If this method returns `true`, calling [`Receiver::recv`] or
    /// [`SerReceiver::try_recv`] will yield until a new message is sent to the
    /// channel. Any calls to [`Receiver::try_recv`] or
    /// [`SerReceiver::try_recv`] while the channel is empty will return
    /// [`TryRecvError::Empty`].
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pipe.core().is_empty()
    }

    /// Returns `true` if this channel is full.
    ///
    /// If this method returns `true`, then any calls to [`Sender::reserve`] or
    /// [`SerSender::reserve`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_reserve`] or [`SerSender::try_reserve`] will return an
    /// error.
    #[inline]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.pipe.core().is_full()
    }

    /// Returns the number of messages currently in the channel.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.pipe.core().len()
    }

    /// Returns the **maximum capacity** of the channel.
    ///
    /// This is the maximum number of messages that may be queued before senders
    /// must wait for additional capacity to become available. The capacity of
    /// the channel is determined *when it is constructed*, and the value
    /// returned by this method will never change over the channel's lifetime,
    /// regardless of the current [length](Self::len) of the channel.
    ///
    /// To determine the current remaining capacity in the channel, use the
    /// [`remaining`](Self::remaining) method, instead.
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.pipe.core().capacity as usize
    }

    /// Returns the **current remaining capacity** of this channel.
    ///
    /// This is equivalent to subtracting [`self.len()`](Self::len) from
    /// [`self.capacity()`](Self::capacity).
    #[inline]
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.len() - self.capacity()
    }
}

impl Drop for SerReceiver {
    fn drop(&mut self) {
        self.pipe.core().close_rx();
    }
}

// === impl SerRecvRef ===

impl SerRecvRef<'_> {
    pub fn to_slice<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        (self.vtable.to_slice)(self.elems, self.res.idx, buf)
    }

    pub fn to_slice_framed<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        (self.vtable.to_slice_framed)(self.elems, self.res.idx, buf)
    }

    /// Serializes the message to an owned `Vec`.
    #[cfg(any(test, feature = "alloc"))]
    pub fn to_vec(&self) -> postcard::Result<alloc::vec::Vec<u8>> {
        (self.vtable.to_vec)(self.elems, self.res.idx)
    }

    /// Returns the serialized representation of the message as a COBS frame, in
    /// an owned `Vec`.
    #[cfg(any(test, feature = "alloc"))]
    pub fn to_vec_framed(&self) -> postcard::Result<alloc::vec::Vec<u8>> {
        (self.vtable.to_vec_framed)(self.elems, self.res.idx)
    }
}

impl fmt::Debug for SerRecvRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            res,
            elems: _,
            vtable,
        } = self;
        f.debug_struct("SerRecvRef")
            .field("res", &res)
            .field("vtable", &format_args!("{vtable:p}"))
            .finish()
    }
}

// === impl SerSender ===

impl SerSender {
    /// Reserve capacity to send a serialized message to the channel.
    ///
    /// If the channel is currently at capacity, this method waits until
    /// capacity for one message is available. When capacity is available,
    /// capacity for one message is reserved for the caller. This method returns
    /// a [`SerPermit`], which represents the reserved capacity. The [`send`]
    /// and [`send_framed`] methods on [`SerPermit`] can be used to send a message,
    /// consuming the reserved capacity.
    ///
    /// Dropping the [`SerPermit`] without sending a message releases the
    /// capacity back to the channel.
    ///
    /// To attempt to reserve capacity *without* waiting if the channel is full,
    /// use the [`try_reserve`] method, instead.
    ///
    /// [`SerPermit`]: SerPermit
    /// [`send`]: SerPermit::send
    /// [`send_framed`]: SerPermit::send_framed
    /// [`try_reserve`]: Self::try_reserve
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`SerPermit`]`)` if the channel is not closed.
    /// - [`Err`]`(`[SendError::Closed`]`)` if the channel is closed (the
    ///   [`Receiver`] or [`SerReceiver`]) has been dropped.
    ///
    /// # Cancellation Safety
    ///
    /// If a `reserve` future is dropped before it has completed, no capacity
    /// will be reserved.
    ///
    /// This channel uses a queue to ensure that calls to `send` and `reserve`
    /// complete in the order they were requested. Cancelling a call to
    /// `reserve` causes the caller to lose its place in that queue.
    pub async fn reserve(&self) -> Result<SerPermit<'_>, SendError> {
        self.pipe.core().reserve().await.map(|res| SerPermit {
            res,
            elems: self.pipe.elems(),
            vtable: self.vtable,
        })
    }

    /// Attempt to reserve capacity to send a serialized message to the channel,
    /// without waiting for capacity to become available.
    ///
    /// If the channel is currently at capacity, this method returns
    /// [`TrySendError::Full`]. If the channel has capacity available, capacity
    /// for one message is reserved for the caller, returning a [`SerPermit`]
    /// which represents the reserved capacity. The [`send`]  and
    /// [`send_framed`] methods on [`SerPermit`] can be used to send a message,
    /// consuming the reserved capacity.
    ///
    /// Dropping the [`SerPermit`] without sending a message releases the
    /// capacity back to the channel.
    ///
    /// To wait for capacity to become available when the channel is full,
    /// rather than returning an error, use the [`reserve`] method, instead.
    ///
    /// [`SerPermit`]: SerPermit
    /// [`send`]: SerPermit::send
    /// [`send_framed`]: SerPermit::send_framed
    /// [`reserve`]: Self::reserve
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`SerPermit`]`)` if the channel has capacity available and
    ///   has not closed.
    /// - [`Err`]`(`[TrySendError::Closed`]`)` if the channel is closed (the
    ///   [`Receiver`] or [`SerReceiver`]) has been dropped. This indicates that
    ///   subsequent calls to `try_reserve` or [`reserve`] on this channel will
    ///   always fail.
    /// - [`Err`]`(`[`TrySendError::Full`]`)` if the channel does not currently
    ///   have capacity to send another message without waiting. A subsequent
    ///   call to `try_reserve` may complete successfully, once capacity has
    ///   become available again.
    pub fn try_reserve(&self) -> Result<SerPermit<'_>, TrySendError> {
        self.pipe.core().try_reserve().map(|res| SerPermit {
            res,
            elems: self.pipe.elems(),
            vtable: self.vtable,
        })
    }

    pub fn try_send(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerTrySendError> {
        self.try_reserve()
            .map_err(SerTrySendError::Send)?
            .send(bytes)
            .map_err(SerTrySendError::Deserialize)
    }

    pub fn try_send_framed(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerTrySendError> {
        self.try_reserve()
            .map_err(SerTrySendError::Send)?
            .send_framed(bytes)
            .map_err(SerTrySendError::Deserialize)
    }

    pub async fn send(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerSendError> {
        self.reserve()
            .await
            .map_err(|_| SerSendError::Closed)?
            .send(bytes)
            .map_err(SerSendError::Deserialize)
    }

    pub async fn send_framed(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerSendError> {
        self.reserve()
            .await
            .map_err(|_| SerSendError::Closed)?
            .send_framed(bytes)
            .map_err(SerSendError::Deserialize)
    }

    /// Returns `true` if this channel is empty.
    ///
    /// If this method returns `true`, calling [`Receiver::recv`] or
    /// [`SerReceiver::try_recv`] will yield until a new message is sent to the
    /// channel. Any calls to [`Receiver::try_recv`] or
    /// [`SerReceiver::try_recv`] while the channel is empty will return
    /// [`TryRecvError::Empty`].
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pipe.core().is_empty()
    }

    /// Returns `true` if this channel is full.
    ///
    /// If this method returns `true`, then any calls to [`Sender::reserve`] or
    /// [`SerSender::reserve`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_reserve`] or [`SerSender::try_reserve`] will return an error.
    #[inline]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.pipe.core().is_full()
    }

    /// Returns the number of messages currently in the channel.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.pipe.core().len()
    }

    /// Returns the **maximum capacity** of the channel.
    ///
    /// This is the maximum number of messages that may be queued before senders
    /// must wait for additional capacity to become available. The capacity of
    /// the channel is determined *when it is constructed*, and the value
    /// returned by this method will never change over the channel's lifetime,
    /// regardless of the current [length](Self::len) of the channel.
    ///
    /// To determine the current remaining capacity in the channel, use the
    /// [`remaining`](Self::remaining) method, instead.
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.pipe.core().capacity as usize
    }

    /// Returns the **current remaining capacity** of this channel.
    ///
    /// This is equivalent to subtracting [`self.len()`](Self::len) from
    /// [`self.capacity()`](Self::capacity).
    #[inline]
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.len() - self.capacity()
    }
}

impl Clone for SerSender {
    fn clone(&self) -> Self {
        self.pipe.core().add_tx();
        Self {
            pipe: self.pipe.clone(),
            vtable: self.vtable,
        }
    }
}

impl Drop for SerSender {
    fn drop(&mut self) {
        self.pipe.core().drop_tx();
    }
}

// === impl SerPermit ===

impl SerPermit<'_> {
    pub fn send(self, bytes: impl AsRef<[u8]>) -> postcard::Result<()> {
        // try to deserialize the bytes into the reserved pipe slot.
        (self.vtable.from_bytes)(self.elems, self.res.idx, bytes.as_ref())?;

        // if we successfully deserialized the bytes, commit the send.
        // otherwise, we'll release the send index when we drop the reservation.
        self.res.commit_send();
        Ok(())
    }

    pub fn send_framed(self, bytes: impl AsRef<[u8]>) -> postcard::Result<()> {
        // try to deserialize the bytes into the reserved pipe slot.
        (self.vtable.from_bytes_framed)(self.elems, self.res.idx, bytes.as_ref())?;

        // if we successfully deserialized the bytes, commit the send.
        // otherwise, we'll release the send index when we drop the reservation.
        self.res.commit_send();
        Ok(())
    }
}
// === impl Sender ===

impl<T> Sender<T> {
    /// Reserve capacity to send a `T`-typed message to the channel.
    ///
    /// If the channel is currently at capacity, this method waits until
    /// capacity for one message is available. When capacity is available,
    /// capacity for one message is reserved for the caller. This method returns
    /// a [`Permit`], which represents the reserved capacity. The [`send`]
    /// and [`commit`] methods on [`Permit`] can be used to send a message,
    /// consuming the reserved capacity.
    ///
    /// Dropping the [`Permit`] without sending a message releases the
    /// capacity back to the channel.
    ///
    /// To attempt to reserve capacity *without* waiting if the channel is full,
    /// use the [`try_reserve`] method, instead.
    ///
    /// [`Permit`]: Permit
    /// [`send`]: Permit::send
    /// [`commit`]: Permit::commit
    /// [`try_reserve`]: Self::try_reserve
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`Permit`]`)` if the channel is not closed.
    /// - [`Err`]`(`[SendError::Closed`]`)` if the channel is closed (the
    ///   [`Receiver`] or [`SerReceiver`]) has been dropped.
    ///
    /// # Cancellation Safety
    ///
    /// If a `reserve` future is dropped before it has completed, no capacity
    /// will be reserved.
    ///
    /// This channel uses a queue to ensure that calls to `send` and `reserve`
    /// complete in the order they were requested. Cancelling a call to
    /// `reserve` causes the caller to lose its place in that queue.
    pub async fn reserve(&self) -> Result<Permit<'_, T>, SendError> {
        let pipe = self.pipe.core().reserve().await?;
        let cell = self.pipe.elems()[pipe.idx as usize].get_mut();
        Ok(Permit { cell, pipe })
    }

    /// Attempt to reserve capacity to send a `T`-typed message to the channel,
    /// without waiting for capacity to become available.
    ///
    /// If the channel is currently at capacity, this method returns
    /// [`TrySendError::Full`]. If the channel has capacity available, capacity
    /// for one message is reserved for the caller, returning a [`Permit`]
    /// which represents the reserved capacity. The [`send`] and [`commit`]
    /// methods on [`Permit`] can be used to send a message, consuming the
    /// reserved capacity.
    ///
    /// Dropping the [`Permit`] without sending a message releases the
    /// capacity back to the channel.
    ///
    /// To wait for capacity to become available when the channel is full,
    /// rather than returning an error, use the [`reserve`] method, instead.
    ///
    /// [`Permit`]: Permit
    /// [`send`]: Permit::send
    /// [`commit`]: Permit::commit
    /// [`reserve`]: Self::reserve
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`Permit`]`)` if the channel has capacity available and
    ///   has not closed.
    /// - [`Err`]`(`[TrySendError::Closed`]`)` if the channel is closed (the
    ///   [`Receiver`] or [`SerReceiver`]) has been dropped. This indicates that
    ///   subsequent calls to `try_reserve` or [`reserve`] on this channel will
    ///   always fail.
    /// - [`Err`]`(`[`TrySendError::Full`]`)` if the channel does not currently
    ///   have capacity to send another message without waiting. A subsequent
    ///   call to `try_reserve` may complete successfully, once capacity has
    ///   become available again.
    pub fn try_reserve(&self) -> Result<Permit<'_, T>, TrySendError> {
        let pipe = self.pipe.core().try_reserve()?;
        let cell = self.pipe.elems()[pipe.idx as usize].get_mut();
        Ok(Permit { cell, pipe })
    }

    /// Returns `true` if this channel is empty.
    ///
    /// If this method returns `true`, calling [`Receiver::recv`] or
    /// [`SerReceiver::try_recv`] will yield until a new message is sent to the
    /// channel. Any calls to [`Receiver::try_recv`] or
    /// [`SerReceiver::try_recv`] while the channel is empty will return
    /// [`TryRecvError::Empty`].
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pipe.core().is_empty()
    }

    /// Returns `true` if this channel is full.
    ///
    /// If this method returns `true`, then any calls to [`Sender::reserve`] or
    /// [`SerSender::reserve`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_reserve`] or [`SerSender::try_send`] will return an error.
    #[inline]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.pipe.core().is_full()
    }

    /// Returns the number of messages currently in the channel.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.pipe.core().len()
    }

    /// Returns the **maximum capacity** of the channel.
    ///
    /// This is the maximum number of messages that may be queued before senders
    /// must wait for additional capacity to become available. The capacity of
    /// the channel is determined *when it is constructed*, and the value
    /// returned by this method will never change over the channel's lifetime,
    /// regardless of the current [length](Self::len) of the channel.
    ///
    /// To determine the current remaining capacity in the channel, use the
    /// [`remaining`](Self::remaining) method, instead.
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.pipe.core().capacity as usize
    }

    /// Returns the **current remaining capacity** of this channel.
    ///
    /// This is equivalent to subtracting [`self.len()`](Self::len) from
    /// [`self.capacity()`](Self::capacity).
    #[inline]
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.len() - self.capacity()
    }
}

impl<T: 'static> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.pipe.core().add_tx();
        Self {
            pipe: self.pipe.clone(),
        }
    }
}

impl<T: 'static> Drop for Sender<T> {
    fn drop(&mut self) {
        self.pipe.core().drop_tx();
    }
}

impl<T> Permit<'_, T> {
    pub fn send(self, val: T) {
        // write the value...
        unsafe {
            // safety: because we allocated the slot's index, we have exclusive
            // mutable access to this slot.
            self.cell.deref().write(val);
        }
        // ...and commit.
        self.pipe.commit_send();
    }

    pub fn commit(self) {
        self.pipe.commit_send();
    }
}

impl<T> fmt::Debug for Permit<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Permit").field(&self.pipe).finish()
    }
}

impl<T> Deref for Permit<'_, T> {
    type Target = MaybeUninit<T>;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.cell.deref() }
    }
}

impl<T> DerefMut for Permit<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.cell.deref() }
    }
}
