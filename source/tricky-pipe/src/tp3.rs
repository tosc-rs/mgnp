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
    channel_core::{
        DeserFn, DeserVtable, ErasedPipe, ErasedSlice, Reservation, SerVtable, TypedPipe,
    },
    error::*,
};

pub use self::static_impl::*;

#[cfg(any(test, feature = "alloc"))]
pub use self::arc_impl::*;

pub struct Receiver<T: 'static> {
    pipe: TypedPipe<T>,
}

pub struct Sender<T: 'static> {
    pipe: TypedPipe<T>,
}

pub struct SerReceiver {
    pipe: ErasedPipe,
    vtable: &'static SerVtable,
}

pub struct SerSender {
    pipe: ErasedPipe,
    vtable: &'static DeserVtable,
}

pub struct SerRecvRef<'pipe> {
    res: Reservation<'pipe>,
    elems: ErasedSlice,
    vtable: &'static SerVtable,
}

pub struct SendRef<'core, T> {
    // load bearing drop ordering lol lmao
    cell: cell::MutPtr<MaybeUninit<T>>,
    pipe: Reservation<'core>,
}

type Cell<T> = UnsafeCell<MaybeUninit<T>>;

// === impl Receiver ===

impl<T> Receiver<T> {
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let res = self.pipe.core().try_dequeue()?;
        let elem =
            self.pipe.elems()[res.idx as usize].with(|ptr| unsafe { (*ptr).assume_init_read() });
        Ok(elem)
    }

    pub async fn recv(&self) -> Result<T, RecvError> {
        loop {
            match self.try_recv() {
                Ok(e) => return Ok(e),
                Err(TryRecvError::Closed) => return Err(RecvError::Closed),
                Err(TryRecvError::Empty) => self
                    .pipe
                    .core()
                    .cons_wait
                    .wait()
                    .await
                    .map_err(|_| RecvError::Closed)?,
            }
        }
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
    /// If this method returns `true`, then any calls to [`Sender::send`] or
    /// [`SerSender::send`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_send`] or [`SerSender`
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
    pub async fn recv(&self) -> Result<SerRecvRef<'_>, RecvError> {
        let res = loop {
            match self.pipe.core().try_dequeue() {
                Ok(res) => break res,
                Err(TryRecvError::Closed) => return Err(RecvError::Closed),
                Err(TryRecvError::Empty) => self
                    .pipe
                    .core()
                    .cons_wait
                    .wait()
                    .await
                    .map_err(|_| RecvError::Closed)?,
            }
        };

        Ok(SerRecvRef {
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
    /// If this method returns `true`, then any calls to [`Sender::send`] or
    /// [`SerSender::send`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_send`] or [`SerSender`
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
    pub fn try_send(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerTrySendError> {
        self.try_send_inner(bytes.as_ref(), self.vtable.from_bytes_framed)
    }

    pub fn try_send_framed(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerTrySendError> {
        self.try_send_inner(bytes.as_ref(), self.vtable.from_bytes_framed)
    }

    pub async fn send(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerSendError> {
        self.send_inner(bytes.as_ref(), self.vtable.from_bytes)
            .await
    }

    pub async fn send_framed(&self, bytes: impl AsRef<[u8]>) -> Result<(), SerSendError> {
        self.send_inner(bytes.as_ref(), self.vtable.from_bytes_framed)
            .await
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
    /// If this method returns `true`, then any calls to [`Sender::send`] or
    /// [`SerSender::send`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_send`] or [`SerSender`
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

    async fn send_inner(&self, bytes: &[u8], deserialize: DeserFn) -> Result<(), SerSendError> {
        loop {
            match self.pipe.core().try_reserve() {
                Ok(res) => {
                    // try writing the bytes to the reservation.
                    deserialize(self.pipe.elems(), res.idx, bytes)
                        .map_err(SerSendError::Deserialize)?;
                    // if we successfully deserialized the bytes, commit the send.
                    // otherwise, we'll release the send index when we drop the reservation.
                    res.commit_send();
                    return Ok(());
                }
                Err(TrySendError::Closed) => return Err(SerSendError::Closed),
                Err(TrySendError::Full) => self
                    .pipe
                    .core()
                    .prod_wait
                    .wait()
                    .await
                    .map_err(|_| SerSendError::Closed)?,
            }
        }
    }

    fn try_send_inner(&self, bytes: &[u8], deserialize: DeserFn) -> Result<(), SerTrySendError> {
        let res = self
            .pipe
            .core()
            .try_reserve()
            .map_err(SerTrySendError::Send)?;
        // try writing the bytes to the reservation.
        deserialize(self.pipe.elems(), res.idx, bytes).map_err(SerTrySendError::Deserialize)?;
        // if we successfully deserialized the bytes, commit the send.
        // otherwise, we'll release the send index when we drop the reservation.
        res.commit_send();
        Ok(())
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

// === impl Sender ===

impl<T> Sender<T> {
    pub fn try_reserve(&self) -> Result<SendRef<'_, T>, TrySendError> {
        let pipe = self.pipe.core().try_reserve()?;
        let cell = self.pipe.elems()[pipe.idx as usize].get_mut();
        Ok(SendRef { cell, pipe })
    }

    pub async fn reserve(&self) -> Result<SendRef<'_, T>, SendError> {
        let pipe = self.pipe.core().reserve().await?;
        let cell = self.pipe.elems()[pipe.idx as usize].get_mut();
        Ok(SendRef { cell, pipe })
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
    /// If this method returns `true`, then any calls to [`Sender::send`] or
    /// [`SerSender::send`] will yield until the queue is empty. Any calls to
    /// [`Sender::try_send`] or [`SerSender`
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

impl<T> SendRef<'_, T> {
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

impl<T> fmt::Debug for SendRef<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SendRef").field(&self.pipe).finish()
    }
}

impl<T> Deref for SendRef<'_, T> {
    type Target = MaybeUninit<T>;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.cell.deref() }
    }
}

impl<T> DerefMut for SendRef<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.cell.deref() }
    }
}
