//! Multi-Producer, Single-Consumer (MPSC), optionally type-erased channels.
//!
//! # Type Erasure
//!
//! The MSPC channels in this module differ from other similar MPSC channels in
//! that they provide multiple mechanisms for erasing the type of a [`Sender`]
//! or [`Receiver`], creating a dynamically-typed sender or receiver handle.
//! These dynamically-typed handles erase the generic type of the channel's
//! messages.
//!
//! There are two forms of channel type erasure:
//!
//! * **Serialization-based type erasure** allows sending or receiving messages
//!   as their `postcard`-serialized byte representations, rather than as Rust
//!   types.
//!
//!   The [`Sender::into_serde`] method converts a [`Sender`] into a
//!   [`DeserSender`]. A [`DeserSender`] provides [`send`](DeserSender::send),
//!   [`try_send`](DeserSender::try_send), and [`reserve`](DeserSender::reserve)
//!   methods that accept a `&[u8]` rather than a typed message, and attempt to
//!   automatically deserialize the channel's message type from the provided bytes.
//!   [`SerReceiver`] handles, respectively.
//!
//!   Similarly, the [`Receiver::into_serde`] method converts a [`Receiver`]
//!   into a [`SerReceiver`], which provides [`recv`](SerReceiver::recv) and
//!   [`try_recv`](SerReceiver::try_recv) methods that return a [`SerRecvRef`]
//!   type. A [`SerRecvRef`] can be used to serialize the received message into
//!   a buffer ([`SerRecvRef::to_slice`]) or into a new [`Vec`](alloc::vec::Vec)
//!   ([`SerRecvRef::to_vec`], if the "alloc" crate feature flag is enabled).
//!
//!   A [`DeserSender`] can only be constructed for a channel where the message
//!   type implements [`serde::de::DeserializeOwned`], and a [`SerReceiver`] may
//!   only be constructed for a channel where the message type implements
//!   [`Serialize`].
//!
//! * **Downcasting-based type erasure** allows converting a typed [`Sender`]
//!   into a type-erased [`ErasedSender`], which can be used to reserve capacity
//!   to send a message to a channel without knowing its message type. The
//!   [`ErasedSender::reserve`] and [`ErasedSender::try_reserve`] methods return
//!   an [`ErasedPermit`], which behaves similarly to a [`dyn
//!   Any`](core::any::Any) value: they may be downcast back to a typed
//!   [`Permit`] using the [`ErasedPermit::downcast`] method.
//!
//!   This is intended to allow code that stores sender handles for a number of
//!   channels with different message types in the same data structure, and
//!   performs runtime type dispatch when sending messages to those channels.
use self::{
    channel_core::{
        CoreVtable, DeserVtable, ErasedPipe, ErasedSlice, Reservation, SerVtable, TypedPipe,
        Vtables,
    },
    error::*,
};
use crate::loom::cell::{self, CellWith, UnsafeCell};
use core::{
    any::TypeId,
    fmt,
    future::Future,
    mem::{self, ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    pin::Pin,
    ptr,
    task::{Context, Poll},
};
use serde::{de::DeserializeOwned, Serialize};

mod channel_core;
pub mod error;

#[cfg(any(test, feature = "alloc"))]
mod arc_impl;

#[cfg(not(loom))]
mod static_impl;

#[cfg(test)]
mod tests;

#[cfg(not(loom))]
pub use self::static_impl::*;

#[cfg(any(test, feature = "alloc"))]
pub use self::arc_impl::*;

/// Receives `T`-typed values from associated [`Sender`]s or [`DeserSender`]s.
///
/// A `Receiver` for a channel can be obtained using the
/// [`StaticTrickyPipe::receiver`] and [`TrickyPipe::receiver`] methods.
pub struct Receiver<T: 'static, E: 'static = ()> {
    pipe: TypedPipe<T, E>,
}

/// Sends `T`-typed values to an associated [`Receiver`]s or [`SerReceiver`].
///
/// A `Sender` for a channel can be obtained using the
/// [`StaticTrickyPipe::sender`] and [`TrickyPipe::sender`] methods.
pub struct Sender<T: 'static, E: 'static = ()> {
    pipe: TypedPipe<T, E>,
}

/// Receives serialized values from associated [`Sender`]s or [`DeserSender`]s.
///
/// A `SerReceiver` for a channel can be obtained using the
/// [`StaticTrickyPipe::ser_receiver`] and [`TrickyPipe::ser_receiver`] methods,
/// when the channel's message type implements [`Serialize`]. Messages may be
/// sent as typed values by a [`Sender`], or as serialized bytes by a [`DeserSender`].
pub struct SerReceiver<E: 'static = ()> {
    pipe: ErasedPipe<E>,
    vtable: &'static SerVtable,
}

/// Sends serialized values to an associated [`Receiver`] or [`SerReceiver`].
///
/// A `DeserSender` for a channel can be obtained using the
/// [`StaticTrickyPipe::deser_sender`] or [`TrickyPipe::deser_sender`] methods,
/// when the channel's message type implements [`DeserializeOwned`]. Messages may be
/// received as deserialized typed values by a [`Receiver`], or as serialized
/// bytes by a [`SerReceiver`].
pub struct DeserSender<E: 'static = ()> {
    pipe: ErasedPipe<E>,
    vtable: &'static DeserVtable,
}

/// An `ErasedSender` can be used to reserve channel capacity while hiding the
/// type of a channel's messages.
///
/// This type provides [`reserve`](Self::reserve) and
/// [`try_reserve`](Self::try_reserve) methods, which behave similarly to the
/// [`Sender::reserve`] and [`Sender::try_reserve`] methods, but return an
/// [`ErasedPermit`], which hides the channel's message type. The
/// [`ErasedPermit`] may later be downcast into a [`Permit`] for the associated
/// channel's message type, using [`ErasedPermit::downcast`], which may be used
/// to send a message.
///
/// See the [module-level documentation on type erasure](super#type-erasure) for
/// details.
pub struct ErasedSender<E: 'static = ()> {
    pipe: ErasedPipe<E>,
}

/// Future returned by [`Receiver::recv`].
///
/// See [the method documentation for `recv`](Receiver::recv) for details.
#[must_use = "futures do nothing unless `.await`ed or `poll`ed"]
#[derive(Debug)]
pub struct Recv<'rx, T: 'static, E: 'static = ()> {
    rx: &'rx Receiver<T, E>,
}

/// Future returned by [`SerReceiver::recv`].
///
/// See [the method documentation for `recv`](SerReceiver::recv) for details.
#[must_use = "futures do nothing unless `.await`ed or `poll`ed"]
#[derive(Debug)]
pub struct SerRecv<'rx, E: 'static = ()> {
    rx: &'rx SerReceiver<E>,
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
pub struct SerRecvRef<'rx, E: 'static = ()> {
    res: Reservation<'rx, E>,
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
pub struct Permit<'tx, T, E> {
    // load bearing drop ordering lol lmao
    cell: cell::MutPtr<MaybeUninit<T>>,
    pipe: Reservation<'tx, E>,
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
#[must_use = "a `SerPermit` does nothing unless the `send` or `send_framed`
              methods are called"]
pub struct SerPermit<'tx, E> {
    res: Reservation<'tx, E>,
    elems: ErasedSlice,
    vtable: &'static DeserVtable,
}

/// A type-erased permit returned by [`ErasedSender::reserve`] and
/// [`ErasedSender::try_reserve`].
///
/// This type may be downcast back into a typed [`Permit`] using the
/// [`ErasedPermit::downcast`] method, which may then be used to send a typed
/// message to the channel.
///
/// See the [module-level documentation on type erasure](super#type-erasure) for
/// details on dynamically type-erased senders.
#[must_use = "a `ErasedPermit` does nothing unless the `downcast` method is called"]
pub struct ErasedPermit<'tx, E: 'static> {
    res: Reservation<'tx, E>,
    elems: ErasedSlice,
    vtable: &'static CoreVtable<E>,
}

type Cell<T> = UnsafeCell<MaybeUninit<T>>;

// === impl Receiver ===

impl<T, E> Receiver<T, E>
where
    E: Clone,
{
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
    /// - [`Err`]`(`[`TryRecvError::Disconnected`]`)` if all [`Sender`]s and
    ///   [`DeserSender`]s have been dropped *and* all messages sent before the
    ///   channel closed have already been received.
    /// - [`Err`]`(`[`TryRecvError::Error`]`(E)` if the channel has been closed
    ///   with an error using the [`Receiver::close_with_error`] or
    ///   [`Sender::close_with_error`] methods.
    /// - [`Err`]`(`[`TryRecvError::Empty`]`)` if there are currently no
    ///   messages in the queue, but the channel has not been closed.
    pub fn try_recv(&self) -> Result<T, TryRecvError<E>> {
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
    /// The [`Future`] returned by this method outputs an error if the channel
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
    /// # Returns
    ///
    /// - [`Ok`]`(T)` if a message was received from the channel.
    /// - [`Err`]`(`[`RecvError::Disconnected`]`)` if all [`Sender`]s and
    ///   [`DeserSender`]s have been dropped *and* all messages sent before the
    ///   channel closed have already been received.
    /// - [`Err`]`(`[`RecvError::Error`]`(E)` if the channel has been closed
    ///   with an error using the [`Receiver::close_with_error`] or
    ///   [`Sender::close_with_error`] methods.
    ///
    /// # Cancellation Safety
    ///
    /// This method is cancel-safe. If `recv` is used as part of a `select!` or
    /// other mechanism for waiting for the first of multiple futures to
    /// complete, and another future completes first, it is guaranteed that no
    /// message will be received from the channel.
    #[inline]
    pub fn recv(&self) -> Recv<'_, T, E> {
        Recv { rx: self }
    }

    /// Polls to receive a message from the channel, returning [`Poll::Ready`]
    /// if a message has been recieved, or [`Poll::Pending`] if there are
    /// currently no messages in the channel.
    ///
    /// # Returns
    ///
    /// - [`Poll::Ready`]`(`[`Ok`]`(T))` if a message was received from the channel.
    /// - [`Poll::Ready`]`(`[`Err`]`(`[`RecvError::Disconnected`]`))` if all
    ///   [`Sender`]s and [`DeserSender`]s have been dropped *and* all messages
    ///   sent before the  channel closed have already been received.
    /// - [`Poll::Ready`]`(`[`Err`]`(`[`TryRecvError::Error`]`(E))` if the channel
    ///   has been closed with an error using the [`Receiver::close_with_error`]
    ///   or [`Sender::close_with_error`] methods.
    /// - [`Poll::Pending`] if there are currently no messages in the queue and
    ///   the calling task should wait for additional messages to be sent.
    pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Result<T, RecvError<E>>> {
        self.pipe
            .core()
            .poll_dequeue(cx)
            .map(|res| Ok(self.take_value(res?)))
    }

    #[inline(always)]
    fn take_value(&self, res: Reservation<'_, E>) -> T {
        self.pipe.elems()[res.idx as usize].with(|ptr| unsafe { (*ptr).assume_init_read() })
    }

    /// Erases the message type of this `Receiver`, returning a [`SerReceiver`]
    /// that receives serialized byte representations of messages.
    pub fn into_serde(self) -> SerReceiver<E>
    where
        T: Serialize + Send + Sync,
    {
        // don't run the destructor for this `Receiver`, as we are converting it
        // into a `DeserReceiver`, which will keep the channel open.
        let this = mem::ManuallyDrop::new(self);
        unsafe {
            // Safety: since we are not dropping the `Receiver`, we can safely
            // duplicate the `TypedPipe`, preserving the existing receiver
            // refcount.
            SerReceiver::new(this.pipe.clone_no_ref_inc())
        }
    }

    /// Close this channel with an error. Any subsequent attempts to send
    /// messages to this channel will fail with `error`.
    ///
    /// This method returns `true` if the channel was successfully closed. If
    /// this channel has already been closed with an error, this method does
    /// nothing and returns `false`.
    pub fn close_with_error(&self, error: E) -> bool {
        self.pipe.core().close_with_error(error)
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

impl<T, E> Drop for Receiver<T, E> {
    fn drop(&mut self) {
        self.pipe.core().close_rx();
    }
}

impl<T, E> fmt::Debug for Receiver<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.pipe.fmt_into(&mut f.debug_struct("Receiver"))
    }
}

impl<T, E: Clone> futures::Stream for &'_ Receiver<T, E> {
    type Item = Result<T, E>;

    #[inline]
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.as_ref().get_ref().poll_recv(cx).map(|res| match res {
            Ok(res) => Some(Ok(res)),
            Err(RecvError::Disconnected) => None,
            Err(RecvError::Error(error)) => Some(Err(error)),
        })
    }
}

impl<T, E: Clone> futures::Stream for Receiver<T, E> {
    type Item = Result<T, E>;

    #[inline]
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.as_ref().get_ref().poll_recv(cx).map(|res| match res {
            Ok(res) => Some(Ok(res)),
            Err(RecvError::Disconnected) => None,
            Err(RecvError::Error(error)) => Some(Err(error)),
        })
    }
}

// === impl SerReceiver ===

impl<E: Clone> SerReceiver<E> {
    fn new<T>(pipe: TypedPipe<T, E>) -> Self
    where
        T: Serialize + Send + 'static,
    {
        Self {
            pipe: unsafe { pipe.erased() },
            vtable: Vtables::<T>::SERIALIZE,
        }
    }

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
    /// - [`Err`]`(`[`TryRecvError::Disconnected`]`)` if all [`Sender`]s and
    ///   [`DeserSender`]s have been dropped) *and* all messages sent before the
    ///   channel closed have already been received.
    /// - [`Err`]`(`[`TryRecvError::Error`]`(E)` if the channel has been closed
    ///   with an error using the [`Receiver::close_with_error`] or
    ///   [`Sender::close_with_error`] methods.
    /// - [`Err`]`(`[`TryRecvError::Empty`]`)` if there are currently no
    ///   messages in the queue, but the channel has not been closed.
    ///
    /// [`Vec`]: alloc::vec::Vec
    pub fn try_recv(&self) -> Result<SerRecvRef<'_, E>, TryRecvError<E>> {
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
    /// [`Result`]`<`[`SerRecvRef`]`, `[`RecvError`]`<E>>`. The future will
    /// complete with an error if the channel has been closed (all [`Sender`]s
    /// and [`DeserSender`]s have been dropped) *and* all messages sent before
    /// the channel closed  have been received, or if the channel is closed with
    /// an error by the user. If the channel has not yet been closed, but there
    /// are no messages currently available in the queue, the [`SerRecv`] future
    /// yields and waits for a new message to be sent, or for the channel to
    /// close.
    ///
    /// To return an error rather than waiting, use the
    /// [`try_recv`](Self::try_recv) method, instead.
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
    /// - [`Err`]`(`[`RecvError::Disconnected`]`)` if all [`Sender`]s and
    ///   [`DeserSender`]s have been dropped *and* all messages sent before the
    ///   channel closed have already been received.
    /// - [`Err`]`(`[`RecvError::Error`]`(E)` if the channel has been closed
    ///   with an error using the [`Receiver::close_with_error`] or
    ///   [`Sender::close_with_error`] methods.
    ///
    /// # Cancellation Safety
    ///
    /// This method is cancel-safe. If `recv` is used as part of a `select!` or
    /// other mechanism for waiting for the first of multiple futures to
    /// complete, and another future completes first, it is guaranteed that no
    /// message will be received from the channel.
    ///
    /// [`Vec`]: alloc::vec::Vec
    pub fn recv(&self) -> SerRecv<'_, E> {
        SerRecv { rx: self }
    }

    /// Polls to receive a serialized message from the channel, returning
    /// [`Poll::Ready`] if a message has been recieved, or [`Poll::Pending`] if
    /// there are currently no messages in the channel.
    ///
    /// # Returns
    ///
    /// - [`Poll::Ready`]`(`[`Ok`]`(`[`SerRecvRef`]`<'_, E>))` if a message was
    ///   received from the channel.
    /// - [`Poll::Ready`]`(`[`Err`]`(`[`RecvError::Disconnected`]`))` if all
    ///   [`Sender`]s and [`DeserSender`]s have been dropped *and* all messages
    ///   sent before the  channel closed have already been received.
    /// - [`Poll::Ready`]`(`[`Err`]`(`[`RecvError::Error`]`(E))` if the channel
    ///   has been closed with an error using the [`Receiver::close_with_error`]
    ///   or [`Sender::close_with_error`] methods.
    /// - [`Poll::Pending`] if there are currently no messages in the queue and
    ///   the calling task should wait for additional messages to be sent.
    pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Result<SerRecvRef<'_, E>, RecvError<E>>> {
        self.pipe.core().poll_dequeue(cx).map(|res| {
            Ok(SerRecvRef {
                res: res?,
                elems: self.pipe.elems(),
                vtable: self.vtable,
            })
        })
    }

    /// Attempts to cast this type-erased `SerReceiver` back to a typed
    /// [`Receiver`]`<T, E>`, if this `SerReceiver` is associated with a channel
    /// of `T`-typed messages.
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`Receiver`]`<T, E>)` if this `SerReceiver` is associated
    ///   with a T`-typed channel.
    /// - [`Err`]`(SerReceiver)` if this `SerReceiver` is associated with a
    ///   channel with a  message type other than `T`, allowing the
    ///   `SerReceiver` to be recovered.
    pub fn downcast<T>(self) -> Result<Receiver<T, E>, Self> {
        // Don't drop this `SerReceiver`, as the return value will own this
        // `SerReceiver`'s reference count.
        let this = ManuallyDrop::new(self);
        match unsafe { this.pipe.clone_no_ref_inc() }.typed::<T>() {
            Some(pipe) => Ok(Receiver { pipe }),
            None => Err(ManuallyDrop::into_inner(this)),
        }
    }

    /// Close this channel with an error. Any subsequent attempts to send
    /// messages to this channel will fail with `error`.
    ///
    /// This method returns `true` if the channel was successfully closed. If
    /// this channel has already been closed with an error, this method does
    /// nothing and returns `false`.
    pub fn close_with_error(&self, error: E) -> bool {
        self.pipe.core().close_with_error(error)
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

impl<E> Drop for SerReceiver<E> {
    fn drop(&mut self) {
        self.pipe.core().close_rx();
    }
}

impl<E> fmt::Debug for SerReceiver<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.pipe.fmt_into(&mut f.debug_struct("SerReceiver"))
    }
}

impl<'rx, E: Clone> futures::Stream for &'rx SerReceiver<E> {
    type Item = Result<SerRecvRef<'rx, E>, E>;

    #[inline]
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.as_ref().get_ref().poll_recv(cx).map(|res| match res {
            Ok(res) => Some(Ok(res)),
            Err(RecvError::Disconnected) => None,
            Err(RecvError::Error(error)) => Some(Err(error)),
        })
    }
}

// === impl Recv ===

impl<T: 'static, E: Clone> Future for Recv<'_, T, E> {
    type Output = Result<T, RecvError<E>>;

    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_recv(cx)
    }
}

// === impl SerRecv ===

impl<'rx, E: Clone> Future for SerRecv<'rx, E> {
    type Output = Result<SerRecvRef<'rx, E>, RecvError<E>>;

    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_recv(cx)
    }
}

// === impl SerRecvRef ===

impl<E> SerRecvRef<'_, E> {
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

impl<E> fmt::Debug for SerRecvRef<'_, E> {
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

impl<E> Drop for SerRecvRef<'_, E> {
    fn drop(&mut self) {
        let Self { res, elems, vtable } = self;
        unsafe {
            // Safety: if a `SerRecvRef` was created for this index, it is
            // assumed to have unique access to the element, and is responsible
            // for dropping it.
            (vtable.drop_elem)(*elems, res.idx);
        }
    }
}

// Safety: this is safe, because a `SerRecvRef` can only be constructed by a
// `SerReceiver`, and `SerReceiver`s may only be constructed for a pipe whose
// messages are `Send`.
unsafe impl<E: Send + Sync> Send for SerRecvRef<'_, E> {}
// Safety: this is safe, because a `SerRecvRef` can only be constructed by a
// `SerReceiver`, and `SerReceiver`s may only be constructed for a pipe whose
// messages are `Send`.
unsafe impl<E: Send + Sync> Sync for SerRecvRef<'_, E> {}

// === impl DeserSender ===

impl<E: Clone> DeserSender<E> {
    fn new<T>(pipe: TypedPipe<T, E>) -> Self
    where
        T: DeserializeOwned + Send + 'static,
    {
        Self {
            pipe: unsafe { pipe.erased() },
            vtable: Vtables::<T>::DESERIALIZE,
        }
    }

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
    /// - [`Err`]`(`[`SendError::Disconnected`]`<()>)` if the [`Receiver`] or
    ///   [`SerReceiver`]) has been dropped.
    /// - [`Err`]`(`[`SendError::Error`]`<E, ()>)` if the channel has been closed
    ///   with an error using the [`Sender::close_with_error`] or
    ///   [`Receiver::close_with_error`] methods. This indicates that subsequent
    ///   calls to [`try_reserve`] or `reserve` on this channel will always fail.
    ///
    /// # Cancellation Safety
    ///
    /// If a `reserve` future is dropped before it has completed, no capacity
    /// will be reserved.
    ///
    /// This channel uses a queue to ensure that calls to `send` and `reserve`
    /// complete in the order they were requested. Cancelling a call to
    /// `reserve` causes the caller to lose its place in that queue.
    pub async fn reserve(&self) -> Result<SerPermit<'_, E>, SendError<E>> {
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
    /// - [`Err`]`(`[`TrySendError::Disconnected`]`<()>)` if the [`Receiver`] or
    ///   [`SerReceiver`] has been dropped. This indicates that subsequent calls
    ///   to `try_reserve` or [`reserve`] on this channel will always fail.
    /// - [`Err`]`(`[`TrySendError::Error`]`<E, ()>)` if the channel has been closed
    ///   with an error using the [`Sender::close_with_error`] or
    ///   [`Receiver::close_with_error`] methods. This indicates that subsequent
    ///   calls to `try_reserve` or [`reserve`] on this channel will always fail.
    /// - [`Err`]`(`[`TrySendError::Full`]`)` if the channel does not currently
    ///   have capacity to send another message without waiting. A subsequent
    ///   call to `try_reserve` may complete successfully, once capacity has
    ///   become available again.
    pub fn try_reserve(&self) -> Result<SerPermit<'_, E>, TrySendError<E>> {
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
    pub fn try_send(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerTrySendError<E>> {
        self.try_reserve()
            .map_err(SerTrySendError::from_try_send_error)?
            .send(bytes)
            .map_err(SerTrySendError::from_ser_send_error)
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
    pub fn try_send_framed(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerTrySendError<E>> {
        self.try_reserve()
            .map_err(SerTrySendError::from_try_send_error)?
            .send_framed(bytes)
            .map_err(SerTrySendError::from_ser_send_error)
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
    pub async fn send(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerSendError<E>> {
        self.reserve()
            .await
            .map_err(SerSendError::from_send_error)?
            .send(bytes)
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
    pub async fn send_framed(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerSendError<E>> {
        self.reserve()
            .await
            .map_err(SerSendError::from_send_error)?
            .send_framed(bytes)
    }

    /// Attempts to cast this type-erased `DeserSender` back to a typed
    /// [`Sender`]`<T, E>`, if this `DeserSender` is associated with a channel of
    /// `T`-typed messages.
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`Sender`]`<T, E>)` if this `DeserSender` is associated with a
    ///   `T`-typed channel.
    /// - [`Err`]`(DeserSender)` if this `DeserSender` is associated with a
    ///   channel with a  message type other than `T`, allowing the `DeserSender` to be recovered.
    pub fn downcast<T>(self) -> Result<Sender<T, E>, Self> {
        // Don't drop this `DeserSender`, as the return value will own this
        // `DeserSender`'s reference count.
        let this = ManuallyDrop::new(self);
        match unsafe { this.pipe.clone_no_ref_inc() }.typed::<T>() {
            Some(pipe) => Ok(Sender { pipe }),
            None => Err(ManuallyDrop::into_inner(this)),
        }
    }

    /// Close this channel with an error. Any subsequent attempts to send
    /// messages to this channel will fail with `error`.
    ///
    /// This method returns `true` if the channel was successfully closed. If
    /// this channel has already been closed with an error, this method does
    /// nothing and returns `false`.
    pub fn close_with_error(&self, error: E) -> bool {
        self.pipe.core().close_with_error(error)
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

impl<E> Clone for DeserSender<E> {
    fn clone(&self) -> Self {
        self.pipe.core().add_tx();
        Self {
            pipe: self.pipe.clone(),
            vtable: self.vtable,
        }
    }
}

impl<E> Drop for DeserSender<E> {
    fn drop(&mut self) {
        self.pipe.core().drop_tx();
    }
}

impl<E> fmt::Debug for DeserSender<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.pipe.fmt_into(&mut f.debug_struct("DeserSender"))
    }
}

// === impl SerPermit ===

impl<E: Clone> SerPermit<'_, E> {
    /// Attempt to send the given bytes
    ///
    /// This will attempt to deserialize the bytes into the reservation, consuming
    /// it. If the deserialization fails, the [SerPermit] is still consumed.
    pub fn send(self, bytes: impl AsRef<[u8]>) -> Result<(), SerSendError<E>> {
        // try to deserialize the bytes into the reserved pipe slot.
        (self.vtable.from_bytes)(self.elems, self.res.idx, bytes.as_ref())
            .map_err(SerSendError::Deserialize)?;

        // if we successfully deserialized the bytes, commit the send.
        // otherwise, we'll release the send index when we drop the reservation.
        self.res
            .commit_send()
            .map_err(SerSendError::from_send_error)
    }

    /// Attempt to send the given bytes
    ///
    /// This will attempt to deserialize the COBS-encoded bytes into the reservation, consuming
    /// it. If the deserialization fails, the [SerPermit] is still consumed.
    pub fn send_framed(self, bytes: impl AsRef<[u8]>) -> Result<(), SerSendError<E>> {
        // try to deserialize the bytes into the reserved pipe slot.
        (self.vtable.from_bytes_framed)(self.elems, self.res.idx, bytes.as_ref())
            .map_err(SerSendError::Deserialize)?;

        // if we successfully deserialized the bytes, commit the send.
        // otherwise, we'll release the send index when we drop the reservation.
        self.res
            .commit_send()
            .map_err(SerSendError::from_send_error)
    }
}

// Safety: this is safe, because a `SerPermit` can only be constructed by a
// `SerSender`, and `SerSender`s may only be constructed for a pipe whose
// messages are `Send`.
unsafe impl<E: Send + Sync> Send for SerPermit<'_, E> {}
// Safety: this is safe, because a `SerPermit` can only be constructed by a
// `SerSender`, and `SerSender`s may only be constructed for a pipe whose
// messages are `Send`.
unsafe impl<E: Send + Sync> Sync for SerPermit<'_, E> {}

// === impl Sender ===

impl<T, E: Clone> Sender<T, E> {
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
    /// - [`Err`]([`SendError::Disconnected`]`<T>`) if the [`Receiver`] or
    ///   [`SerReceiver`]) has been dropped.
    /// - [`Err`]`(`[`SendError::Error`]`<E, T>)` if the channel has been closed
    ///   with an error using the [`Sender::close_with_error`] or
    ///   [`Receiver::close_with_error`] methods. This indicates that subsequent
    ///   calls to `send` or [`try_send`] on this channel will always fail.
    ///
    /// # Cancellation Safety
    ///
    /// If a `send` future is dropped before it has completed, no capacity
    /// will be reserved.
    ///
    /// This channel uses a queue to ensure that calls to `send` and `reserve`
    /// complete in the order they were requested. Cancelling a call to
    /// `send` causes the caller to lose its place in that queue.
    pub async fn send(&self, message: T) -> Result<(), SendError<E, T>> {
        match self.reserve().await {
            Ok(permit) => {
                permit.send(message);
                Ok(())
            }
            Err(err) => Err(err.with_message(message)),
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
    /// - [`Err`]([`TrySendError::Disconnected`]`<T>`) if the [`Receiver`] or
    ///   [`SerReceiver`]) has been dropped. This indicates that subsequent
    ///   calls to [`send`], `try_send`, [`try_reserve`], or [`reserve`] on this
    ///   channel will always fail.
    /// - [`Err`]([`TrySendError::Error`]`<E, T>`) if the channel has been closed
    ///   with an error using the [`Sender::close_with_error`] or
    ///   [`Receiver::close_with_error`] methods. This indicates that subsequent
    ///   calls to [`send`], `try_send`, [`try_reserve`], or [`reserve`] on this
    ///   channel will always fail.
    /// - [`Err`]`(`[`TrySendError::Full`]`)` if the channel does not currently
    ///   have capacity to send another message without waiting. A subsequent
    ///   call to `try_reserve` may complete successfully, once capacity has
    ///   become available again.
    ///
    /// [`send`]: Self::send
    /// [`reserve`]: Self::reserve
    /// [`try_reserve`]: Self::try_reserve
    pub fn try_send(&self, message: T) -> Result<(), TrySendError<E, T>> {
        match self.try_reserve() {
            Ok(permit) => {
                permit.send(message);
                Ok(())
            }
            Err(e) => Err(e.with_message(message)),
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
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`Permit`]`)` if the channel is not closed.
    /// - [`Err`]([`SendError::Disconnected`]`<()>`) if the [`Receiver`] or
    ///   [`SerReceiver`]) has been dropped.
    /// - [`Err`]`(`[`SendError::Error`]`<E, ()>)` if the channel has been closed
    ///   with an error using the [`Sender::close_with_error`] or
    ///   [`Receiver::close_with_error`] methods. This indicates that subsequent
    ///   calls to `reserve`, [`try_reserve`], [`send`](Self::send),
    ///   [`try_send`] on this  channel will always fail.
    ///
    /// # Cancellation Safety
    ///
    /// If a `reserve` future is dropped before it has completed, no capacity
    /// will be reserved.
    ///
    /// This channel uses a queue to ensure that calls to `send` and `reserve`
    /// complete in the order they were requested. Cancelling a call to
    /// `reserve` causes the caller to lose its place in that queue.
    ///
    /// [`Permit`]: Permit
    /// [`send`]: Permit::send
    /// [`try_send`]: Self::try_send
    /// [`commit`]: Permit::commit
    /// [`try_reserve`]: Self::try_reserve
    pub async fn reserve(&self) -> Result<Permit<'_, T, E>, SendError<E>> {
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
    /// - [`Err`]([`TrySendError::Disconnected`]`<()>`) if the [`Receiver`] or
    ///   [`SerReceiver`]) has been dropped. This indicates that subsequent
    ///   calls to [`send`], `try_send`, [`try_reserve`], or [`reserve`] on this
    ///   channel will always fail.
    /// - [`Err`]([`TrySendError::Error`]`<E, ()>`) if the channel has been closed
    ///   with an error using the [`Sender::close_with_error`] or
    ///   [`Receiver::close_with_error`] methods. This indicates that subsequent
    ///   calls to [`send`](Self::send), `try_send`, [`try_reserve`], or
    ///   [`reserve`] on this  channel will always fail.
    /// - [`Err`]`(`[`TrySendError::Full`]`)` if the channel does not currently
    ///   have capacity to send another message without waiting. A subsequent
    ///   call to `try_reserve` may complete successfully, once capacity has
    ///   become available again.
    ///
    /// [`reserve`]: Self::reserve
    /// [`try_reserve`]: Self::try_reserve
    pub fn try_reserve(&self) -> Result<Permit<'_, T, E>, TrySendError<E>> {
        let pipe = self.pipe.core().try_reserve()?;
        let cell = self.pipe.elems()[pipe.idx as usize].get_mut();
        Ok(Permit { cell, pipe })
    }

    /// Erases the message type of this `Sender`, returning a [`DeserSender`]
    /// that sends messages from their serialized binary representations.
    pub fn into_serde(self) -> DeserSender<E>
    where
        T: DeserializeOwned + Send + Sync,
    {
        // don't run the destructor for this `Sender`, as we are converting it
        // into a `SerSender`, keeping the existing reference count held by
        // this `Sender`.
        let this = mem::ManuallyDrop::new(self);
        DeserSender::new(unsafe {
            // Safety: since we are not dropping the `Sender`, we can safely
            // duplicate the `TypedPipe`, preserving the existing receiver
            // refcount.
            this.pipe.clone_no_ref_inc()
        })
    }

    /// Erases the message type of this `Sender`, returning a [`ErasedSender`]
    /// which is not generic over `T`.
    ///
    /// An [`ErasedSender`] may be used to reserve channel capacity without
    /// requiring the channel's message type to be known, returning a
    /// type-erased [`ErasedPermit`]. The [`ErasedPermit`] may then be downcast back
    /// into a [`Permit`] and used to send a message, using
    /// [`ErasedPermit::downcast`].
    pub fn into_erased(self) -> ErasedSender<E>
    where
        T: Send + Sync + 'static,
    {
        // don't run the destructor for this `Sender`, as we are converting it
        // into an `ErasedSender`, keeping the existing reference count held by
        // this `Sender`.
        let this = mem::ManuallyDrop::new(self);
        ErasedSender::new(unsafe {
            // Safety: since we are not dropping the `Sender`, we can safely
            // duplicate the `TypedPipe`, preserving the existing receiver
            // refcount.
            this.pipe.clone_no_ref_inc()
        })
    }

    /// Close this channel with an error. Any subsequent attempts to send
    /// messages to this channel will fail with `error`.
    ///
    /// This method returns `true` if the channel was successfully closed. If
    /// this channel has already been closed with an error, this method does
    /// nothing and returns `false`.
    pub fn close_with_error(&self, error: E) -> bool {
        self.pipe.core().close_with_error(error)
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

impl<T: 'static, E> Clone for Sender<T, E> {
    fn clone(&self) -> Self {
        self.pipe.core().add_tx();
        Self {
            pipe: self.pipe.clone(),
        }
    }
}

impl<T: 'static, E> Drop for Sender<T, E> {
    fn drop(&mut self) {
        self.pipe.core().drop_tx();
    }
}

impl<T: 'static, E> fmt::Debug for Sender<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.pipe.fmt_into(&mut f.debug_struct("Sender"))
    }
}

// === impl Permit ===

impl<T, E: Clone> Permit<'_, T, E> {
    /// Write the given value into the [Permit], and send it.
    ///
    /// This makes the data available to the [Receiver].
    ///
    /// Capacity for the message has already been reserved. The message is sent
    /// to the receiver and the permit is consumed. The operation will succeed
    /// even if the receiver half has been closed.
    pub fn send(self, val: T) {
        // write the value...
        unsafe {
            // safety: because we allocated the slot's index, we have exclusive
            // mutable access to this slot.
            self.cell.deref().write(val);

            // ...and commit.
            self.commit()
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

        // ignore errors here because capacity is already reserved.
        let _ = self.pipe.commit_send();
    }
}

impl<T, E> fmt::Debug for Permit<'_, T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Permit").field(&self.pipe).finish()
    }
}

impl<T, E> Deref for Permit<'_, T, E> {
    type Target = MaybeUninit<T>;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.cell.deref() }
    }
}

impl<T, E> DerefMut for Permit<'_, T, E> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.cell.deref() }
    }
}

// Safety: a `Permit` allows referencing a `T`, so it's morally equivalent to a
// reference: a `Permit` is `Send` if `T` is `Send + Sync`.
unsafe impl<T: Send + Sync, E: Send + Sync> Send for Permit<'_, T, E> {}

// Safety: a `Permit` allows referencing a `T`, so it's morally equivalent to a
// reference: a `Permit` is `Sync` if `T` is `Sync`.
unsafe impl<T: Sync, E: Send + Sync> Sync for Permit<'_, T, E> {}

// === impl ErasedSender ===

impl<E: Clone> ErasedSender<E> {
    fn new<T>(pipe: TypedPipe<T, E>) -> Self
    where
        T: Send + Sync + 'static,
    {
        Self {
            pipe: unsafe { pipe.erased() },
        }
    }

    /// Reserve capacity to send a message to the channel.
    ///
    /// If the channel is currently at capacity, this method waits until
    /// capacity for one message is available. When capacity is available,
    /// capacity for one message is reserved for the caller. This method returns
    /// a [`ErasedPermit`], which represents the reserved capacity. The
    /// [`downcast`] method on the returned [`ErasedPermit`] can be used to
    /// downcast the [`ErasedPermit`] back into a [`Permit`], which may be used to
    /// send a message to the channel using the [`Permit::send`] method.
    ///
    /// Dropping the [`ErasedPermit`] without sending a message releases the
    /// capacity back to the channel.
    ///
    /// To attempt to reserve capacity *without* waiting if the channel is full,
    /// use the [`try_reserve`] method, instead.
    ///
    /// [`ErasedPermit`]: ErasedPermit
    /// [`downcast`]: ErasedPermit::downcast
    /// [`try_reserve`]: Self::try_reserve
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`ErasedPermit`]`)` if the channel is not closed.
    /// - [`Err`]`(`[`SendError::Disconnected`]`<()>)` if the [`Receiver`] or
    ///   [`SerReceiver`]) has been dropped.
    /// - [`Err`]`(`[`SendError::Error`]`<E, ()>)` if the channel has been closed
    ///   with an error using the [`Sender::close_with_error`] or
    ///   [`Receiver::close_with_error`] methods. This indicates that subsequent
    ///   calls to [`try_reserve`] or `reserve` on this channel will always fail.
    ///
    /// # Cancellation Safety
    ///
    /// If a `reserve` future is dropped before it has completed, no capacity
    /// will be reserved.
    ///
    /// This channel uses a queue to ensure that calls to `send` and `reserve`
    /// complete in the order they were requested. Cancelling a call to
    /// `reserve` causes the caller to lose its place in that queue.
    pub async fn reserve(&self) -> Result<ErasedPermit<'_, E>, SendError<E>> {
        self.pipe.core().reserve().await.map(|res| ErasedPermit {
            res,
            elems: self.pipe.elems(),
            vtable: self.pipe.vtable,
        })
    }

    /// Attempt to reserve capacity to send a message to the channel,
    /// without waiting for capacity to become available.
    ///
    /// If the channel is currently at capacity, this method returns
    /// [`TrySendError::Full`]. If the channel has capacity available, capacity
    /// for one message is reserved for the caller, returning a [`ErasedPermit`]
    /// which represents the reserved capacity. The [`downcast`] method on the
    /// returned [`ErasedPermit`] can be used to downcast the [`ErasedPermit`] back
    /// into a [`Permit`], which may be used to send a message to the channel
    /// using the [`Permit::send`] method.
    ///
    /// Dropping the [`ErasedPermit`] without sending a message releases the
    /// capacity back to the channel.
    ///
    /// To wait for capacity to become available when the channel is full,
    /// rather than returning an error, use the [`reserve`] method, instead.
    ///
    /// [`ErasedPermit`]: SerPermit
    /// [`downcast`]: ErasedPermit::downcast
    /// [`reserve`]: Self::reserve
    ///
    /// # Returns
    ///
    /// - [`Ok`]`(`[`ErasedPermit`]`)` if the channel has capacity available and
    ///   has not closed.
    /// - [`Err`]`(`[`TrySendError::Disconnected`]`<()>)` if the [`Receiver`] or
    ///   [`SerReceiver`] has been dropped. This indicates that subsequent calls
    ///   to `try_reserve` or [`reserve`] on this channel will always fail.
    /// - [`Err`]`(`[`TrySendError::Error`]`<E, ()>)` if the channel has been closed
    ///   with an error using the [`Sender::close_with_error`] or
    ///   [`Receiver::close_with_error`] methods. This indicates that subsequent
    ///   calls to `try_reserve` or [`reserve`] on this channel will always fail.
    /// - [`Err`]`(`[`TrySendError::Full`]`)` if the channel does not currently
    ///   have capacity to send another message without waiting. A subsequent
    ///   call to `try_reserve` may complete successfully, once capacity has
    ///   become available again.
    pub fn try_reserve(&self) -> Result<ErasedPermit<'_, E>, TrySendError<E>> {
        self.pipe.core().try_reserve().map(|res| ErasedPermit {
            res,
            elems: self.pipe.elems(),
            vtable: self.pipe.vtable,
        })
    }

    /// Returns `true` if this `ErasedSender` is associated with a channel of
    /// `T`-typed messages, or `false` if the channel's message type is not `T`.
    ///
    /// If this method returns `true`, then calling
    /// [`ErasedPermit::downcast::<T>()`](ErasedPermit::downcast) on the
    /// [`ErasedPermit`]s returned by this `ErasedSender`'s [`reserve`] or
    /// [`try_reserve`] methods will return [`Ok`]`(`[`Permit`]`<'_, T, E>)`. If
    /// this method returns `false` for a given `T`, then downcasting
    /// [`ErasedPermit`]s returned by this sender to a `T` will return an
    /// [`Err`].
    ///
    /// [`reserve`]: Self::reserve
    /// [`try_reserve`]: Self::try_reserve
    #[inline]
    #[must_use]
    pub fn is<T>(&self) -> bool
    where
        T: Send + Sync + 'static,
    {
        self.type_id() == TypeId::of::<T>()
    }

    /// Returns the [`TypeId`] of the type of messages sent by this
    /// `ErasedSender`.
    #[inline]
    #[must_use]
    pub fn type_id(&self) -> TypeId {
        (self.pipe.vtable.type_id)()
    }

    /// Returns a `&'static str` representation of the type of messages sent by
    /// this `ErasedSender`.
    #[inline]
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        (self.pipe.vtable.type_name)()
    }

    /// Close this channel with an error. Any subsequent attempts to send
    /// messages to this channel will fail with `error`.
    ///
    /// This method returns `true` if the channel was successfully closed. If
    /// this channel has already been closed with an error, this method does
    /// nothing and returns `false`.
    pub fn close_with_error(&self, error: E) -> bool {
        self.pipe.core().close_with_error(error)
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

impl<E> Clone for ErasedSender<E> {
    fn clone(&self) -> Self {
        self.pipe.core().add_tx();
        Self {
            pipe: self.pipe.clone(),
        }
    }
}

impl<E> Drop for ErasedSender<E> {
    fn drop(&mut self) {
        self.pipe.core().drop_tx();
    }
}

impl<E> fmt::Debug for ErasedSender<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.pipe.fmt_into(&mut f.debug_struct("ErasedSender"))
    }
}

// === impl ErasedPermit ===

impl<'tx, E> ErasedPermit<'tx, E> {
    /// Attempts to downcast this `ErasedPermit to a `T`-typed [`Permit`].
    ///
    /// If this `ErasedPermit` was produced by a channel of `T`-typed messages,
    /// then this method returns an [`Ok`]`(`[`Permit`]`<'_, T, E>)`, which may
    /// be used to send a `T`-typed message to the channel. Otherwise, if the
    /// message type of this channel is not `T`, this method returns an [`Err`]
    /// containing the original `ErasedPermit`, so that it may be downcast to a
    /// different channel type.
    pub fn downcast<T>(self) -> Result<Permit<'tx, T, E>, Self>
    where
        T: Send + Sync + 'static,
    {
        if !self.is::<T>() {
            return Err(self);
        }

        let elems = unsafe {
            // Safety: we just checked that the requested type matches the type
            // of the erased slice, so unerasing it to that type is okay.
            self.elems.unerase::<Cell<T>>()
        };
        let cell = elems[self.res.idx as usize].get_mut();
        Ok(Permit {
            cell,
            pipe: self.res,
        })
    }

    /// Returns `true` if this `ErasedPermit` is associated with a channel of
    /// `T`-typed messages, or `false` if the channel's message type is not `T`.
    ///
    /// If this method returns `true`, then [`self.downcast::<T>()`] will return
    /// [`Ok`]`(`[`Permit`]`<'_, T, E>)`. If this method returns `false`, then
    /// [`self.downcast::<T>()`] will return an [`Err`].
    ///
    /// [`self.downcast::<T>()`]: Self::downcast
    #[inline]
    #[must_use]
    pub fn is<T>(&self) -> bool
    where
        T: Send + Sync + 'static,
    {
        self.type_id() == TypeId::of::<T>()
    }

    /// Returns the [`TypeId`] of the type of messages sent by this
    /// [`ErasedPermit`].
    #[inline]
    #[must_use]
    pub fn type_id(&self) -> TypeId {
        (self.vtable.type_id)()
    }

    /// Returns a `&'static str` representation of the type of messages sent by
    /// this `ErasedPermit`.
    #[inline]
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        (self.vtable.type_name)()
    }
}

impl<E> fmt::Debug for ErasedPermit<'_, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ErasedPermit")
            .field("res", &self.res)
            .field("type", &format_args!("{}", self.type_name()))
            .finish()
    }
}

// Safety: an ``ErasedPermit` can only be constructed if `T` is `Send + Sync`.
unsafe impl<E: Send + Sync> Send for ErasedPermit<'_, E> {}

// Safety: an ``ErasedPermit` can only be constructed if `T` is `Send + Sync`.
unsafe impl<E: Send + Sync> Sync for ErasedPermit<'_, E> {}
