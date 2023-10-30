//! Tricky Pipe
//!
//! Tricky Pipe is a channel that has interchangeable ends to allow for
//! transparent serialization and deserialization.
//!
//! It is intended to be used in cases where you *sometimes* need to traverse
//! a network hop (or similar), and Serialization or Deserialization may occur.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![warn(rustdoc::broken_intra_doc_links)]
#![warn(missing_docs)]

#[cfg(any(feature = "alloc", test, loom))]
extern crate alloc;

use crate::loom::cell::{self, UnsafeCell};
use core::{
    fmt,
    future::Future,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr,
    task::{ready, Context, Poll},
};
use serde::{de::DeserializeOwned, Serialize};
pub mod bidi;

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
                const EXPR: &str = stringify!($x);
                tracing::event!(tracing::Level::DEBUG, { EXPR } = ?format_args!("{x:#?}"));
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
        tracing::info!($($arg)*);
    };
}

#[cfg(not(test))]
macro_rules! test_span {
    ($($arg:tt)*) => {};
}

#[cfg(all(test, not(loom)))]
macro_rules! test_span {
    ($($arg:tt)*) => {
        let _span = tracing::debug_span!($($arg)*).entered();
    };
}

#[cfg(all(test, loom))]
macro_rules! test_span {
    ($($arg:tt)*) => {
        tracing::info!(message = $($arg)*);
    };
}

#[cfg(any(test, feature = "alloc"))]
mod arc_impl;
mod channel_core;
pub mod error;
#[cfg(not(loom))]
mod static_impl;
#[cfg(test)]
mod tests;

use self::{
    channel_core::{DeserVtable, ErasedPipe, ErasedSlice, Reservation, SerVtable, TypedPipe},
    error::*,
};

#[cfg(not(loom))]
pub use self::static_impl::*;

#[cfg(any(test, feature = "alloc"))]
pub use self::arc_impl::*;

/// Receives `T`-typed values from associated [`Sender`]s or [`DeserSender`]s.
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

/// Receives serialized values from associated [`Sender`]s or [`DeserSender`]s.
///
/// A `SerReceiver` for a channel can be obtained using the
/// [`StaticTrickyPipe::ser_receiver`] and [`TrickyPipe::ser_receiver`] methods,
/// when the channel's message type implements [`Serialize`]. Messages may be
/// sent as typed values by a [`Sender`], or as serialized bytes by a [`DeserSender`].
pub struct SerReceiver {
    pipe: ErasedPipe,
    vtable: &'static SerVtable,
}

/// Sends serialized values to an associated [`Receiver`] or [`SerReceiver`].
///
/// A `DeserSender` for a channel can be obtained using the
/// [`StaticTrickyPipe::deser_sender`] or [`TrickyPipe::deser_sender`] methods,
/// when the channel's message type implements [`DeserializeOwned`]. Messages may be
/// received as deserialized typed values by a [`Receiver`], or as serialized
/// bytes by a [`SerReceiver`].
pub struct DeserSender {
    pipe: ErasedPipe,
    vtable: &'static DeserVtable,
}

/// Future returned by [`Receiver::recv`].
///
/// See [the method documentation for `recv`](Receiver::recv) for details.
#[must_use = "futures do nothing unless `.await`ed or `poll`ed"]
#[derive(Debug)]
pub struct Recv<'rx, T: 'static> {
    rx: &'rx Receiver<T>,
}

/// Future returned by [`SerReceiver::recv`].
///
/// See [the method documentation for `recv`](SerReceiver::recv) for details.
#[must_use = "futures do nothing unless `.await`ed or `poll`ed"]
#[derive(Debug)]
pub struct SerRecv<'rx> {
    rx: &'rx SerReceiver,
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
/// This type is returned by the [`DeserSender::try_reserve`] and
/// [`DeserSender::reserve`] methods.
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
    /// - [`Err`]`(`[`TryRecvError::Closed`]`) if the channel has been closed
    ///   (all [`Sender`]s and [`DeserSender`]s have been dropped) *and* all
    ///   messages sent before the channel closed have already been received.
    /// - [`Err`]`(`[`TryRecvError::Empty`]`)` if there are currently no
    ///   messages in the queue, but the channel has not been closed.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.pipe
            .core()
            .try_dequeue()
            .map(|res| self.take_value(res))
    }

    /// Receives the next message from the channel, returning a [`Recv`] [`Future`].
    ///
    /// This is equivalent to:
    /// ```
    /// # struct Receiver<T>(T);
    /// # impl<T> Receiver<T> {
    /// pub async fn recv(&self) -> Option<T>
    /// # { None }
    /// # }
    /// ```
    ///
    /// The [`Future`] returned by this method outputs [`None`] if the channel
    /// has been closed (all [`Sender`]s and [`DeserSender`]s have been dropped)
    /// *and* all messages sent before the channel closed have been received.
    ///
    /// If the channel has not yet been closed, but there are no messages
    /// currently available in the queue, the [`Future`] yields and waits for a
    /// new message to be sent, or for the channel to close.
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
    #[inline]
    pub fn recv(&self) -> Recv<'_, T> {
        Recv { rx: self }
    }

    /// Polls to receive a message from the channel, returning [`Poll::Ready`]
    /// if a message has been recieved, or [`Poll::Pending`] if there are
    /// currently no messages in the channel.
    pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.pipe
            .core()
            .poll_dequeue(cx)
            .map(|res| Some(self.take_value(res?)))
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
    /// [`DeserSender::reserve`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_reserve`] or [`DeserSender::try_reserve`] will return an
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

impl<T> fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.pipe.fmt_into(&mut f.debug_struct("Receiver"))
    }
}

impl<T> futures::Stream for &'_ Receiver<T> {
    type Item = T;

    #[inline]
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.as_ref().get_ref().poll_recv(cx)
    }
}

impl<T> futures::Stream for Receiver<T> {
    type Item = T;

    #[inline]
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.as_ref().get_ref().poll_recv(cx)
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
    /// - [`Err`]`([`TryRecvError::Closed`]) if the channel has been closed
    ///   (all [`Sender`]s and [`DeserSender`]s have been dropped) *and* all
    ///   messages sent before the channel closed have already been received.
    /// - [`Err`]`([`TryRecvError::Empty`]) if there are currently no
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
    /// This is equivalent to:
    /// ```
    /// # struct SerRecvRef<'rx>(&'rx ());
    /// # struct SerReceiver;
    /// # impl SerReceiver {
    /// pub async fn recv(&self) -> Option<SerRecvRef<'_>>
    /// # { None }
    /// # }
    /// ```
    ///
    /// This method returns a [`SerRecv`] [`Future`] that outputs an
    /// [`Option`]`<`[`SerRecvRef`]`>`. The future will complete with [`None`]
    /// if the channel has been closed (all [`Sender`]s and [`DeserSender`]s
    /// have been dropped) *and* all messages sent before the channel closed
    /// have been received. If the channel has not yet been closed, but there
    /// are no messages currently available in the queue, the [`SerRecv`] future
    /// yields and waits for a new message to be sent, or for the channel to
    /// close.
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
    pub fn recv(&self) -> SerRecv<'_> {
        SerRecv { rx: self }
    }

    /// Polls to receive a serialized message from the channel, returning
    /// [`Poll::Ready`] if a message has been recieved, or [`Poll::Pending`] if
    /// there are currently no messages in the channel.
    pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Option<SerRecvRef<'_>>> {
        self.pipe.core().poll_dequeue(cx).map(|res| {
            Some(SerRecvRef {
                res: res?,
                elems: self.pipe.elems(),
                vtable: self.vtable,
            })
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
    /// [`DeserSender::reserve`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_reserve`] or [`DeserSender::try_reserve`] will return an
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

impl fmt::Debug for SerReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.pipe.fmt_into(&mut f.debug_struct("SerReceiver"))
    }
}

impl<'rx> futures::Stream for &'rx SerReceiver {
    type Item = SerRecvRef<'rx>;

    #[inline]
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.as_ref().get_ref().poll_recv(cx)
    }
}

// === impl Recv ===

impl<T: 'static> Future for Recv<'_, T> {
    type Output = Option<T>;

    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_recv(cx)
    }
}

// === impl SerRecv ===

impl<'rx> Future for SerRecv<'rx> {
    type Output = Option<SerRecvRef<'rx>>;

    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_recv(cx)
    }
}

// === impl SerRecvRef ===

impl SerRecvRef<'_> {
    /// Attempt to serialize the received item into the provided buffer
    ///
    /// This function will fail if the provided buffer was too small.
    pub fn to_slice<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        (self.vtable.to_slice)(self.elems, self.res.idx, buf)
    }

    /// Attempt to serialize the received item into the provided buffer, using COBS encoding
    ///
    /// This function will fail if the provided buffer was too small.
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

// Safety: this is safe, because a `SerRecvRef` can only be constructed by a
// `SerReceiver`, and `SerReceiver`s may only be constructed for a pipe whose
// messages are `Send`.
unsafe impl Send for SerRecvRef<'_> {}
// Safety: this is safe, because a `SerRecvRef` can only be constructed by a
// `SerReceiver`, and `SerReceiver`s may only be constructed for a pipe whose
// messages are `Send`.
unsafe impl Sync for SerRecvRef<'_> {}

// === impl DeserSender ===

impl DeserSender {
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
    /// - [`Err`]`(`[SendError`]`<()>)` if the channel is closed (the
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
    pub async fn reserve(&self) -> Result<SerPermit<'_>, SendError<()>> {
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

    /// Attempt to immediately send the given bytes
    ///
    /// This will attempt to reserve space for a message in the queue, and then deserialize
    /// the bytes into that reservation.
    ///
    /// If space is not immediately available, an error will be returned.
    ///
    /// This is equivalent to calling [DeserSender::try_reserve] followed by
    /// [SerPermit::send].
    pub fn try_send(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerTrySendError> {
        self.try_reserve()
            .map_err(SerTrySendError::Send)?
            .send(bytes)
            .map_err(SerTrySendError::Deserialize)
    }

    /// Attempt to immediately send the given framed bytes
    ///
    /// This will attempt to reserve space for a message in the queue, and then deserialize
    /// the COBS encoded bytes into that reservation.
    ///
    /// If space is not immediately available, an error will be returned.
    ///
    /// This is equivalent to calling [DeserSender::try_reserve] followed by
    /// [SerPermit::send_framed].
    pub fn try_send_framed(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerTrySendError> {
        self.try_reserve()
            .map_err(SerTrySendError::Send)?
            .send_framed(bytes)
            .map_err(SerTrySendError::Deserialize)
    }

    /// Attempt to send the given bytes
    ///
    /// This will attempt to reserve space for a message in the queue, and then deserialize
    /// the bytes into that reservation.
    ///
    /// This function will yield until space is available, or until the channel is closed.
    ///
    /// This is equivalent to calling [DeserSender::reserve] followed by
    /// [SerPermit::send].
    pub async fn send(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerSendError> {
        self.reserve()
            .await
            .map_err(|_| SerSendError::Closed)?
            .send(bytes)
            .map_err(SerSendError::Deserialize)
    }

    /// Attempt to  send the given framed bytes
    ///
    /// This will attempt to reserve space for a message in the queue, and then deserialize
    /// the COBS encoded bytes into that reservation.
    ///
    /// This function will yield until space is available, or until the channel is closed.
    ///
    /// This is equivalent to calling [DeserSender::reserve] followed by
    /// [SerPermit::send_framed].
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
    /// [`DeserSender::reserve`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_reserve`] or [`DeserSender::try_reserve`] will return an error.
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

impl Clone for DeserSender {
    fn clone(&self) -> Self {
        self.pipe.core().add_tx();
        Self {
            pipe: self.pipe.clone(),
            vtable: self.vtable,
        }
    }
}

impl Drop for DeserSender {
    fn drop(&mut self) {
        self.pipe.core().drop_tx();
    }
}

impl fmt::Debug for DeserSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.pipe.fmt_into(&mut f.debug_struct("DeserSender"))
    }
}
// === impl SerPermit ===

impl SerPermit<'_> {
    /// Attempt to send the given bytes
    ///
    /// This will attempt to deserialize the bytes into the reservation, consuming
    /// it. If the deserialization fails, the [SerPermit] is still consumed.
    pub fn send(self, bytes: impl AsRef<[u8]>) -> postcard::Result<()> {
        // try to deserialize the bytes into the reserved pipe slot.
        (self.vtable.from_bytes)(self.elems, self.res.idx, bytes.as_ref())?;

        // if we successfully deserialized the bytes, commit the send.
        // otherwise, we'll release the send index when we drop the reservation.
        self.res.commit_send();
        Ok(())
    }

    /// Attempt to send the given bytes
    ///
    /// This will attempt to deserialize the COBS-encoded bytes into the reservation, consuming
    /// it. If the deserialization fails, the [SerPermit] is still consumed.
    pub fn send_framed(self, bytes: impl AsRef<[u8]>) -> postcard::Result<()> {
        // try to deserialize the bytes into the reserved pipe slot.
        (self.vtable.from_bytes_framed)(self.elems, self.res.idx, bytes.as_ref())?;

        // if we successfully deserialized the bytes, commit the send.
        // otherwise, we'll release the send index when we drop the reservation.
        self.res.commit_send();
        Ok(())
    }
}

// Safety: this is safe, because a `SerPermit` can only be constructed by a
// `SerSender`, and `SerSender`s may only be constructed for a pipe whose
// messages are `Send`.
unsafe impl Send for SerPermit<'_> {}
// Safety: this is safe, because a `SerPermit` can only be constructed by a
// `SerSender`, and `SerSender`s may only be constructed for a pipe whose
// messages are `Send`.
unsafe impl Sync for SerPermit<'_> {}

// === impl Sender ===

impl<T> Sender<T> {
    /// Send a `T`-typed message to the channel.
    ///
    /// If the channel is currently at capacity, this method waits until
    /// capacity for one message is available. When capacity is available, the
    /// provided message is sent to the channel.
    ///
    /// To attempt to send a message *without* waiting if the channel is full,
    /// use the [`try_send`] method, instead.
    ///
    /// [`try_send`]: Self::try_send
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`()`]`)` if the channel is not closed.
    /// - [`Err`]([1SendError`]`<T>`) if the channel is closed (the
    ///   [`Receiver`] or [`SerReceiver`]) has been dropped.
    ///
    /// # Cancellation Safety
    ///
    /// If a `send` future is dropped before it has completed, no capacity
    /// will be reserved.
    ///
    /// This channel uses a queue to ensure that calls to `send` and `reserve`
    /// complete in the order they were requested. Cancelling a call to
    /// `send` causes the caller to lose its place in that queue.
    pub async fn send(&self, message: T) -> Result<(), SendError<T>> {
        match self.reserve().await {
            Ok(permit) => {
                permit.send(message);
                Ok(())
            }
            Err(_) => Err(SendError(message)),
        }
    }

    /// Attempt to send a `T`-typed message to the channel, without waiting for
    /// capacity to become available.
    ///
    /// If the channel is currently at capacity, this method returns
    /// [`TrySendError::Full`]. If the channel has capacity available, the
    /// message is sent to the channel immediately.
    ///
    /// To wait for capacity to become available when the channel is full,
    /// rather than returning an error, use the [`send`] method, instead.
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(())` if the message was sent successfully.
    /// - [`Err`]([`TrySendError::Closed`]`<T>`) if the channel is closed (the
    ///   [`Receiver`] or [`SerReceiver`]) has been dropped. This indicates that
    ///   subsequent calls to [`send`], `try_send`, [`try_reserve`], or
    ///   [`reserve`] on this channel will always fail.
    /// - [`Err`]`(`[`TrySendError::Full`]`)` if the channel does not currently
    ///   have capacity to send another message without waiting. A subsequent
    ///   call to `try_reserve` may complete successfully, once capacity has
    ///   become available again.
    ///
    /// [`send`]: Self::send
    /// [`reserve`]: Self::reserve
    /// [`try_reserve`]: Self::try_reserve
    pub fn try_send(&self, message: T) -> Result<(), TrySendError<T>> {
        match self.try_reserve() {
            Ok(permit) => {
                permit.send(message);
                Ok(())
            }
            Err(TrySendError::Closed(())) => Err(TrySendError::Closed(message)),
            Err(TrySendError::Full(())) => Err(TrySendError::Full(message)),
        }
    }

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
    /// [`DeserSender::reserve`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_reserve`] or [`DeserSender::try_send`] will return an error.
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

impl<T: 'static> fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.pipe.fmt_into(&mut f.debug_struct("Sender"))
    }
}

// === impl Permit ===

impl<T> Permit<'_, T> {
    /// Write the given value into the [Permit], and send it
    ///
    /// This makes the data available to the [Receiver].
    pub fn send(self, val: T) {
        // write the value...
        unsafe {
            // safety: because we allocated the slot's index, we have exclusive
            // mutable access to this slot.
            self.cell.deref().write(val);

            // ...and commit.
            self.commit();
        }
    }

    /// Send the current value of the reserved slot to the channel.
    ///
    /// This method is intended to be used in conjunction with the
    /// [`DerefMut`]`<Target = `[`MaybeUninit`]`<T>>` implementation for
    /// `Permit` to allow writing to the reserved slot in the channel's buffer
    /// in place, rather than by moving a value into the slot. This may, in some
    /// cases, be more efficient when the messages are large.
    ///
    /// # Safety
    ///
    /// Calling `commit` without writing to the reserved slot **will result in
    /// the [`Receiver`] reading uninitialized memory**! Ensure that the slot
    /// has been initialized prior to calling this method!
    #[inline]
    pub unsafe fn commit(self) {
        // the write is over, make sure `loom` knows we're done with the mutable
        // pointer *before* we actually release the slot to the receiver.
        #[cfg_attr(not(loom), allow(clippy::drop_non_drop))]
        drop(self.cell);

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

// Safety: a `Permit` allows referencing a `T`, so it's morally equivalent to a
// reference: a `Permit` is `Send` if `T` is `Send + Sync`.
unsafe impl<T: Send + Sync> Send for Permit<'_, T> {}

// Safety: a `Permit` allows referencing a `T`, so it's morally equivalent to a
// reference: a `Permit` is `Sync` if `T` is `Sync`.
unsafe impl<T: Sync> Sync for Permit<'_, T> {}

pub(crate) mod loom;
