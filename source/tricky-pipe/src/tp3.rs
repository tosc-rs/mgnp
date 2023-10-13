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
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.pipe
            .core()
            .try_dequeue()
            .map(|res| self.take_value(res))
    }

    /// Receives the next value from the channel.
    ///
    /// This method returns [`None`] if the channel has been closed (all
    /// [`Sender`]s and [`SerSender`]s have been dropped) *and* all messages
    /// sent before the channel closed have been received.
    ///
    /// If the channel has not yet been closed, but there are no messages
    /// currently available in the queue, this method yields and waits for a new
    /// message to be sent, or for the channel to close. To return an error
    /// rather than waiting, use the [`try_recv`] method, instead.
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
    /// Attempt to receive a message from the channel, if there are currently
    /// any messages in the channel.
    ///
    /// This method returns a [`SerRecvRef`] which may be used to serialize the
    /// message.
    pub fn try_recv(&self) -> Result<SerRecvRef<'_>, TryRecvError> {
        let res = self.pipe.core().try_dequeue()?;
        Ok(SerRecvRef {
            res,
            elems: self.pipe.elems(),
            vtable: self.vtable,
        })
    }

    /// Receive the next message from the channel, waiting for one to be sent if
    /// the channel is empty.
    ///
    /// This method returns a [`SerRecvRef`] which may be used to serialize the
    /// received message.
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
    pub async fn reserve(&self) -> Result<SerPermit<'_>, SendError> {
        self.pipe.core().reserve().await.map(|res| SerPermit {
            res,
            elems: self.pipe.elems(),
            vtable: self.vtable,
        })
    }

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
    pub fn try_reserve(&self) -> Result<Permit<'_, T>, TrySendError> {
        let pipe = self.pipe.core().try_reserve()?;
        let cell = self.pipe.elems()[pipe.idx as usize].get_mut();
        Ok(Permit { cell, pipe })
    }

    pub async fn reserve(&self) -> Result<Permit<'_, T>, SendError> {
        let pipe = self.pipe.core().reserve().await?;
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
