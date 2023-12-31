//! # One-Shot Channels
//!
//! A one-shot channel is used for sending a single message between asynchronous
//! tasks. One-shot channels consist of paired [`Receiver`] and [`Sender`] (or
//! [`DeserSender`]) handles. The [`Receiver`] is used to await a message from
//! the [`Sender`], while the [`Sender`] sends a single message using its
//! [`send`](Sender::send) method. A new one-shot channel is constructed using the
//! [`Receiver::new`] function.
//!
//! ## Reusing a One-Shot Channel
//!
//! Unlike the one-shot channels provided by other async channel libraries, this
//! one-shot channel is _reusable_. A single [`Receiver`] may create a
//! [`Sender`] or [`DeserSender`] multiple times, using the [`Receiver::sender`]
//! and [`Receiver::deser_sender`] methods, respectively. This allows the same
//! `static` or heap allocation to be reused for multiple messages. However,
//! unlike a multi-producer, single-consumer (MPSC) channel, only a single
//! [`Sender`] handle may exist at any time. If a [`Sender`] or [`DeserSender`]
//! has been created  but has not yet been used, the [`Receiver::sender`] and
//! [`Receiver::deser_sender`] will fail until that sender has been used to send
//! a message.
//!
//! ## Heap and Static Storage
//!
//! In order to construct a one-shot channel, storage must exist for the
//! state that is shared between the [`Receiver`] and any [`Sender`]
//! or [`DeserSender`]s that the [`Receiver`] creates. This shared state is
//! represented by the [`Oneshot`] type.
//!
//! The storage must be valid for the
//! `'static` lifetime. Shared state may be stored either on the heap using an
//! [`Arc`], if the "alloc" feature flag is enabled, or in a `static`.
//!
//! ### `static` Storage
//!
//! The [`Oneshot`] may be stored in a `static` binding, ensuring it is never
//! deallocated. This allows creating a [`Receiver`] for the channel using
//! [`Oneshot::static_receiver`].
//!
//! Because no heap allocations are used, this storage mechanism is available
//! for use on embedded systems which lack a heap or dynamic allocation.
//!
//! For example:
//!
//! ```
//! use tricky_pipe::oneshot;
//!
//! static CHAN: oneshot::Oneshot<usize> = oneshot::Oneshot::new();
//! # #[tokio::main(flavor = "current_thread")] async fn main() {
//!
//! // because the `Oneshot` is stored in a `static`, we can create a
//! // `Receiver` using the `static_receiver` method:
//! let rx = CHAN.static_receiver().unwrap();
//! // now that the receiver exists, we can create a sender:
//! let tx = rx.sender().await.unwrap();
//!
//! tx.send(1).unwrap();
//! assert_eq!(rx.recv().await, Ok(1));
//! # }
//! ```
//!
//! ### [`Arc`] Storage
//!
//! To allow dynamic allocation of one-shot channels, the [`Oneshot`] may
//! instead be stored in an [`Arc`]. This allows sharing a clone of that [`Arc`]
//! with the sender side of the channel. A [`Receiver`] can be created using the
//! [`Oneshot::arc_receiver`] method. Alternatively, an [`Arc`]-based
//! [`Receiver`] can also be constructed using [`Receiver::new`].
//!
//! This storage approach requires `liballoc`, and these methods are only
//! available when the "alloc" crate feature flag is enabled.
//!
//! For example:
//!
//! ```
//! use tricky_pipe::oneshot;
//! use std::sync::Arc;
//!
//! # #[tokio::main(flavor = "current_thread")] async fn main() {
//! let chan = Arc::new(oneshot::Oneshot::new());
//!
//! // because the `Oneshot` is stored in an `Arc`, we can create a
//! // `Receiver` using the `arc_receiver` method:
//! let rx = chan.arc_receiver().unwrap();
//! // now that the receiver exists, we can create a sender:
//! let tx = rx.sender().await.unwrap();
//!
//! tx.send(1).unwrap();
//! assert_eq!(rx.recv().await, Ok(1));
//! # }
//! ```
//!
//! Alternatively, [`Receiver::new`] can be used to avoid explicitly
//! constructing an [`Arc`]`<`[`Oneshot`]`<T>>`:
//!
//! ```
//! use tricky_pipe::oneshot;
//!
//! # #[tokio::main(flavor = "current_thread")] async fn main() {
//! let rx = oneshot::Receiver::new();
//! // now that the receiver exists, we can create a sender:
//! let tx = rx.sender().await.unwrap();
//!
//! tx.send(1).unwrap();
//! assert_eq!(rx.recv().await, Ok(1));
//! # }
#![warn(missing_debug_implementations)]
use crate::{
    loom::{
        cell::{CellWith, UnsafeCell},
        hint,
        sync::atomic::{AtomicU8, Ordering::*},
    },
    typeinfo::TypeInfo,
};
use core::{
    any::TypeId,
    fmt,
    future::Future,
    mem::{self, MaybeUninit},
    pin::Pin,
    task::{self, Context, Poll},
};
use serde::de::DeserializeOwned;

use maitake_sync::WaitCell;

#[cfg(any(test, feature = "alloc"))]
use alloc::sync::Arc;

/// A reusable one-shot channel.
///
/// Essentially, a one-shot channel is a single producer, single consumer
/// channel, with a maximum capacity of one message. Many producers can be
/// created over the lifecycle of a single consumer. However, only zero or one
/// producers can be live at any given time
///
/// A [`Receiver`]`<T>` can be used to hand out single-use [`Sender`] or
/// [`DeserSender`] handles, using the [`Receiver::sender`] and
/// [`Receiver::deser_sender`] methods. Each sender handle can be used once
/// to send a single message to the receiver.
///
/// See the [module-level documentation](../#reusing-a-one-shot-channel) for
/// details on reusing a [`Receiver`].
#[repr(C)]
#[must_use = "a `Receiver` does nothing unless used to receive a message"]
pub struct Receiver<T> {
    chan: *const Oneshot<T>,
    vtable: &'static RefVtable,
}

/// A type-erased reusable one-shot channel.
pub struct ErasedReceiver {
    chan: *const Oneshot<()>,
    vtable: &'static RefVtable,
    type_id: TypeId,
    drop_data: unsafe fn(*const Oneshot<()>),
}

/// Sends a single message to the corresponding [`Receiver`].
///
/// This handle provides the ability to send one `T`-typed message to a
/// [`Receiver`], using the [`Sender::send`] method.
///
/// While this [`Sender`] exists, no other [`Sender`]s or [`DeserSender`]s may
/// be created. Dropping this [`Sender`] releases the reservation on the
/// channel, allowing a new [`Sender`] or [`DeserSender`] to be created.
///
/// [`Sender`]s are constructed using the [`Receiver::sender`]  method.
#[must_use = "a `Sender` does nothing unless used to send a message"]
pub struct Sender<T> {
    chan: *const Oneshot<T>,
    vtable: &'static RefVtable,
    sent: bool,
}

/// Deserializes a single message and sends it to the corresponding
/// [`Receiver`].
///
/// This type-erased handle provides the ability to send one serialized message
/// to a [`Receiver`], using the [`DeserSender::send`] or
/// [`DeserSender::send_framed`]. Unlike the [`Sender`] type, this sender's send
/// methods take a serialized message as a byte slice and automatically
/// deserialize it, if it is of the type expected by the [`Receiver`].
///
/// While this [`DeserSender`] exists, no other [`Sender`]s or [`DeserSender`]s
/// may be created. Dropping this [`DeserSender`] releases the reservation on the
/// channel, allowing a new [`Sender`] or [`DeserSender`] to be created.
///
/// [`DeserSender`]s are constructed using the [`Receiver::deser_sender`]
/// method.
#[must_use = "a `DeserSender` does nothing unless used to receive a message"]
pub struct DeserSender {
    chan: *const Oneshot<()>,
    vtable: &'static RefVtable,
    ser_vtable: &'static DeserVtable,
    sent: bool,
}

/// Future returned by [`Receiver::recv`].
#[must_use = "futures do nothing unless `.await`ed or `poll`ed"]
#[derive(Debug)]
pub struct Recv<'rx, T> {
    rx: &'rx Receiver<T>,
}

/// Future returned by [`ErasedReceiver::recv_erased`].
#[must_use = "futures do nothing unless `.await`ed or `poll`ed"]
#[derive(Debug)]
pub struct RecvErased<'rx> {
    rx: &'rx ErasedReceiver,
}

/// A type-erased reference to a value received from an [`ErasedReceiver`].
#[derive(Debug)]
pub struct ErasedRecvRef<'rx> {
    rx: &'rx ErasedReceiver,
    taken: bool,
}

/// Errors returned by [`Receiver::recv`] and [`Receiver::poll_recv`].
#[derive(Debug, Eq, PartialEq)]
pub enum RecvError {
    /// No [`Sender`] or [`DeserSender`] currently exists, so a message could
    /// not be received.
    NoSender,
    /// The [`Receiver`] has closed the channel using the
    /// [`close`](Receiver::close) method, so no messages can be received.
    Closed,
}

/// Errors returned by [`Receiver::sender`] and [`Receiver::deser_sender`].
#[derive(Debug, Eq, PartialEq)]
pub enum SenderError {
    /// A [`Sender`] or [`DeserSender`] already exists, so a new one may not be
    /// created at this time.
    ///
    /// When the currently active [`Sender`] or [`DeserSender`] is dropped, the
    /// next call to the [`Receiver::sender`] or [`Receiver::deser_sender`]
    /// methods on this [`Receiver`] will succeed.
    SenderAlreadyActive,
    /// The [`Receiver`] has closed the channel using the
    /// [`close`](Receiver::close) method, so no senders can be created.
    ///
    /// If this error is returned, the [`Receiver::sender`] and
    /// [`Receiver::deser_sender`] methods on this [`Receiver`] will *never*
    /// return [`Ok`] again.
    Closed,
}

/// Errors returned by [`ErasedReceiver::downcast_sender`].
#[derive(Debug, Eq, PartialEq)]
pub enum ErasedSenderError {
    /// A sender could not be acquired.
    Sender(SenderError),
    /// The `ErasedReceiver` is of a different type.
    WrongType,
}

/// Errors returned by [`Sender::send`], indicating that the [`Receiver`] has
/// closed the channel using the  [`close`](Receiver::close) method, so no
/// messages can be sent.
///
/// The message that was sent is returned so that it may be reused.
#[derive(Debug, Eq, PartialEq)]
pub struct Closed<T>(T);

/// Errors returned by [`DeserSender::send`] and [`DeserSender::send_framed`].
#[derive(Debug, Eq, PartialEq)]
pub enum DeserSendError {
    /// The message could not be deserialized.
    Deserialize {
        /// The deserialization error returned by `postcard`.
        error: postcard::Error,
        /// The type name of the message type expected by the sender.
        message_type: &'static str,
    },
    /// The [`Receiver`] has closed the channel using the
    /// [`close`](Receiver::close) method, so no messages can be sent.
    Closed,
}

/// Shared storage used by both ends of a oneshot channel.
#[repr(C)]
pub struct Oneshot<T> {
    head: Header,
    cell: UnsafeCell<MaybeUninit<T>>,
}

#[derive(Debug)]
struct Header {
    state: AtomicU8,
    wait: WaitCell,
}

struct DeserVtable {
    drop_data: fn(&DeserSender),
    from_bytes: fn(&DeserSender, &[u8]) -> postcard::Result<()>,
    from_bytes_framed: fn(&DeserSender, &mut [u8]) -> postcard::Result<()>,
    typeinfo: TypeInfo,
}

#[derive(Debug)]
struct RefVtable {
    drop: unsafe fn(*const Oneshot<()>),
    clone: unsafe fn(*const Oneshot<()>),
}

/// Not waiting for anything.
const HAS_RX: u8 = 1 << 0;
/// A Sender has been created, but no writes have begun yet
const HAS_TX: u8 = 1 << 1;
/// The receiver is waiting for a value.
const RX_WAITING: u8 = 1 << 2;
// A value has been sent.
const SENT: u8 = 1 << 3;
/// The Oneshot has been manually closed or dropped.
const CLOSED: u8 = 1 << 4;

impl<T> Oneshot<T> {
    /// Returns a new `Oneshot`.
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        Self {
            head: Header {
                state: AtomicU8::new(0),
                wait: WaitCell::new(),
            },
            cell: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    #[cfg(any(test, feature = "alloc"))]
    const ARC_VTABLE: &'static RefVtable = &RefVtable {
        drop: |ptr| unsafe { Arc::decrement_strong_count(ptr.cast::<Self>()) },
        clone: |ptr| unsafe { Arc::increment_strong_count(ptr.cast::<Self>()) },
    };

    const STATIC_VTABLE: &'static RefVtable = &RefVtable {
        drop: |_| {},
        clone: |_| {},
    };

    /// Constructs a [`Receiver`] for this `Oneshot` channel, using a `static`
    /// to store the shared state between the [`Receiver`] and any [`Sender`]s.
    ///
    /// If a [`Receiver`] has already been created for this channel, this method
    /// returns [`None`].
    ///
    /// # Examples
    ///
    /// ```
    /// use tricky_pipe::oneshot;
    ///
    /// static CHAN: oneshot::Oneshot<usize> = oneshot::Oneshot::new();
    /// # #[tokio::main(flavor = "current_thread")] async fn main() {
    ///
    /// // because the `Oneshot` is stored in a `static`, we can create a
    /// // `Receiver` using the `static_receiver` method:
    /// let rx = CHAN.static_receiver().unwrap();
    /// // now that the receiver exists, we can create a sender:
    /// let tx = rx.sender().await.unwrap();
    ///
    /// tx.send(1).unwrap();
    /// assert_eq!(rx.recv().await, Ok(1));
    /// # }
    /// ```
    pub fn static_receiver(&'static self) -> Option<Receiver<T>> {
        self.head
            .state
            .compare_exchange(0, HAS_RX, AcqRel, Acquire)
            .ok()?;
        Some(Receiver {
            chan: self as *const _,
            vtable: Self::STATIC_VTABLE,
        })
    }

    /// Constructs a [`Receiver`] for this `Oneshot` channel, using an [`Arc`]
    /// to store the shared state between the [`Receiver`] and any [`Sender`]s.
    ///
    /// If a [`Receiver`] has already been created for this channel, this method
    /// returns [`None`].
    ///
    /// This method requires the "alloc" feature flag to be enabled. When using
    /// `Arc`s to store the shared state, the [`Receiver::new`] function may be
    /// used to avoid constructing the  [`Oneshot`] separately from the
    /// [`Receiver`].
    ///
    /// # Examples
    ///
    /// ```
    /// use tricky_pipe::oneshot;
    /// use std::sync::Arc;
    ///
    /// # #[tokio::main(flavor = "current_thread")] async fn main() {
    /// let chan = Arc::new(oneshot::Oneshot::new());
    ///
    /// // because the `Oneshot` is stored in an `Arc`, we can create a
    /// // `Receiver` using the `arc_receiver` method:
    /// let rx = chan.arc_receiver().unwrap();
    /// // now that the receiver exists, we can create a sender:
    /// let tx = rx.sender().await.unwrap();
    ///
    /// tx.send(1).unwrap();
    /// assert_eq!(rx.recv().await, Ok(1));
    /// # }
    /// ```
    #[cfg(any(test, feature = "alloc"))]
    pub fn arc_receiver(self: Arc<Self>) -> Option<Receiver<T>> {
        self.head
            .state
            .compare_exchange(0, HAS_RX, AcqRel, Acquire)
            .ok()?;
        Some(Receiver::from_arc(self))
    }

    fn poll_recv(&self, cx: &mut task::Context<'_>) -> Poll<Result<(), RecvError>> {
        test_span!("Oneshot::poll_recv");
        loop {
            let state = test_dbg!(self.head.state.fetch_or(RX_WAITING, AcqRel));

            if test_dbg!(state & CLOSED != 0) {
                return Poll::Ready(Err(RecvError::Closed));
            }

            if test_dbg!(state & SENT != 0) {
                return Poll::Ready(Ok(()));
            }

            if test_dbg!(state & HAS_TX == 0) {
                return Poll::Ready(Err(RecvError::NoSender));
            }

            // We are still waiting for the Sender to start or complete.
            // Trigger another wait cycle.
            task::ready!(test_dbg!(self.head.wait.poll_wait(cx))).map_err(|_| RecvError::Closed)?;
            hint::spin_loop();
        }
    }

    unsafe fn take_value(&self) -> T {
        let mut ret = MaybeUninit::<T>::uninit();
        self.cell.with_mut(|cell| {
            core::ptr::copy_nonoverlapping(cell.cast(), ret.as_mut_ptr(), 1);
        });

        test_dbg!(self.head.state.store(HAS_RX, Release));
        ret.assume_init()
    }
}

impl<T> fmt::Debug for Oneshot<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Oneshot")
            .field("head", &self.head)
            .field(
                "cell",
                &format_args!("UnsafeCell<{}>", core::any::type_name::<T>()),
            )
            .finish()
    }
}

unsafe impl<T: Send> Send for Oneshot<T> {}
unsafe impl<T: Send> Sync for Oneshot<T> {}

// === impl Receiver ===

impl<T> Receiver<T> {
    /// Returns a new one-shot channel receiver, wrapped in an `Arc`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tricky_pipe::oneshot;
    ///
    /// # #[tokio::main(flavor = "current_thread")] async fn main() {
    /// let rx = oneshot::Receiver::new();
    /// // now that the receiver exists, we can create a sender:
    /// let tx = rx.sender().await.unwrap();
    ///
    /// tx.send(1).unwrap();
    /// assert_eq!(rx.recv().await, Ok(1));
    /// # }
    #[cfg(any(test, feature = "alloc"))]
    pub fn new() -> Self {
        Self::from_arc(Arc::new(Oneshot {
            head: Header {
                state: AtomicU8::new(HAS_RX),
                wait: WaitCell::new(),
            },
            cell: UnsafeCell::new(MaybeUninit::uninit()),
        }))
    }

    #[cfg(any(test, feature = "alloc"))]
    fn from_arc(oneshot: Arc<Oneshot<T>>) -> Self {
        Self {
            chan: Arc::into_raw(oneshot),
            vtable: Oneshot::<T>::ARC_VTABLE,
        }
    }

    /// Create a [`Sender`] for this channel.
    ///
    /// If a [`Sender`] or [`DeserSender`] currently exists and has not been
    /// used, this method returns a [`SenderError`]. If a message has been sent
    /// but not received, this method will call [`Receiver::recv`] to receive
    /// that message, drop it, and then create a new [`Sender`].
    ///
    ///
    /// # Examples
    ///
    /// ```
    /// use tricky_pipe::oneshot;
    /// use std::sync::Arc;
    ///
    /// # #[tokio::main(flavor = "current_thread")] async fn main() {
    /// let rx = Arc::new(oneshot::Receiver::new());
    ///
    /// // because the `Receiver` is stored in an `Arc`, we can create a
    /// // `Sender` using the `sender` method:
    /// let tx = rx.sender().await.unwrap();
    ///
    /// tx.send(1).unwrap();
    /// assert_eq!(rx.recv().await, Ok(1));
    /// # }
    /// ```
    pub async fn sender(&self) -> Result<Sender<T>, SenderError> {
        self.take_sender().await?;
        unsafe { (self.vtable.clone)(self.chan.cast()) }
        Ok(Sender {
            chan: self.chan,
            vtable: self.vtable,
            sent: false,
        })
    }

    async fn take_sender(&self) -> Result<(), SenderError> {
        test_span!("Oneshot::sender");
        let this = unsafe { &*self.chan };
        while let Err(state) =
            test_dbg!(this
                .head
                .state
                .compare_exchange(HAS_RX, HAS_RX | HAS_TX, AcqRel, Acquire))
        {
            if state & SENT != 0 {
                let _ = test_dbg!(self.recv().await.map(|_| ()));
            }

            if state & CLOSED != 0 {
                return Err(SenderError::Closed);
            }

            if state & HAS_TX != 0 {
                return Err(SenderError::SenderAlreadyActive);
            }
        }

        Ok(())
    }

    /// Await a message from a [`Sender`] or [`DeserSender`].
    ///
    /// If a sender has not been created, this function will immediately return
    /// [`RecvError::NoSender`]. If the sender is dropped without sending a
    /// response, this function will return [`RecvError::NoSender`] after the
    /// sender has been dropped.
    pub fn recv(&self) -> Recv<'_, T> {
        Recv { rx: self }
    }

    /// Await a message from a [`Sender`] or [`DeserSender`].
    pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Result<T, RecvError>> {
        let this = unsafe { &*self.chan };
        this.poll_recv(cx).map_ok(|_| unsafe { this.take_value() })
    }

    /// Close the Oneshot. This will cause any pending senders to fail.
    pub fn close(&self) {
        let this = unsafe { &*self.chan };
        if this.head.close() {
            this.cell
                .with_mut(|cell| unsafe { core::ptr::drop_in_place(cell.cast::<T>()) });
        }
    }

    /// Convert this [`Receiver`]`<T>` into a type-erased [`ErasedReceiver`],
    /// which erases the generic message type.
    ///
    /// The [`ErasedReceiver::recv_erased`] method returns an [`ErasedRecvRef`],
    /// which may then be dynamically downcast to the original message type.
    pub fn into_erased(self) -> ErasedReceiver
    where
        T: Send + 'static,
    {
        let this = mem::ManuallyDrop::new(self);
        ErasedReceiver {
            chan: this.chan.cast(),
            vtable: this.vtable,
            type_id: TypeId::of::<T>(),
            drop_data: |chan| unsafe {
                let chan = chan.cast::<Oneshot<T>>();
                (*chan).cell.with_mut(|cell| {
                    core::ptr::drop_in_place(cell.cast::<T>());
                });
            },
        }
    }
}

impl<T> Receiver<T>
where
    T: DeserializeOwned + Send + 'static,
{
    const DESER_VTABLE: DeserVtable = DeserVtable {
        drop_data: |this| unsafe {
            this.ser_vtable
                .typeinfo
                .assert_matches::<T>("oneshot::DeserSender");
            let chan = this.chan.cast::<Oneshot<T>>();
            (*chan).cell.with_mut(|cell| {
                core::ptr::drop_in_place(cell.cast::<T>());
            });
        },
        from_bytes: |this, bytes| -> postcard::Result<()> {
            this.ser_vtable
                .typeinfo
                .assert_matches::<T>("oneshot::DeserSender");
            let val = postcard::from_bytes::<T>(bytes)?;
            let chan = this.chan.cast::<Oneshot<T>>();
            unsafe {
                (*chan).cell.with_mut(|cell| {
                    core::ptr::write(cell.cast::<T>(), val);
                });
            };
            Ok(())
        },
        from_bytes_framed: |this, bytes| {
            this.ser_vtable
                .typeinfo
                .assert_matches::<T>("oneshot::DeserSender");
            let val = postcard::from_bytes_cobs::<T>(bytes)?;
            let chan = this.chan.cast::<Oneshot<T>>();
            unsafe {
                (*chan).cell.with_mut(|cell| {
                    core::ptr::write(cell.cast::<T>(), val);
                });
            };
            Ok(())
        },
        typeinfo: TypeInfo::of::<T>(),
    };

    /// Create a type-erased, deserializing [`DeserSender`] for this channel, if
    /// no sender currently exists.
    ///
    /// If a [`Sender`] or [`DeserSender`] currently exists and has not been
    /// used, this method returns a [`SenderError`]. If a message has been sent
    /// but not received, this method will call [`Receiver::recv`] to receive
    /// that message, drop it, and then create a new [` DeserSender`].
    pub async fn deser_sender(&self) -> Result<DeserSender, SenderError> {
        self.take_sender().await?;
        unsafe {
            (self.vtable.clone)(self.chan.cast());
        }
        Ok(DeserSender {
            chan: self.chan.cast::<Oneshot<()>>(),
            vtable: self.vtable,
            ser_vtable: &Self::DESER_VTABLE,
            sent: false,
        })
    }
}

impl<T> fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("oneshot::Receiver")
            .field("chan", &format_args!("{:p}", self.chan))
            .finish()
    }
}

#[cfg(any(test, feature = "alloc"))]
impl<T> Default for Receiver<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        unsafe {
            // decrement the ref count, if this sender was constructed from an
            // `Arc<Oneshot>`.
            (self.vtable.drop)(self.chan.cast())
        }
    }
}

unsafe impl<T: Send> Send for Receiver<T> {}
unsafe impl<T: Send> Sync for Receiver<T> {}

// === impl ErasedReceiver ===

impl ErasedReceiver {
    /// Create a [`Sender`] for this channel, if it was erased from a
    /// [`Receiver`] for `T`-typed messages.
    ///
    /// If a [`Sender`] or [`DeserSender`] currently exists and has not been
    /// used, this method returns a [`ErasedSenderError::Sender`]. If a message
    /// has been sent but not received, this method will call
    /// [`ErasedReceiver::recv_erased`] to receive
    /// that message, drop it, and then create a new [`Sender`]. Finally, if
    /// this `ErasedReceiver` was *not* erased from a [`Receiver`]`<T>`, this
    /// method returns [`ErasedSenderError::WrongType`].
    ///
    /// # Examples
    ///
    /// Downcasting to a matching type:
    ///
    /// ```
    /// use tricky_pipe::oneshot;
    ///
    /// # #[tokio::main(flavor = "current_thread")] async fn main() {
    /// let rx = oneshot::Receiver::<usize>::new().into_erased();
    /// // Because the `Receiver` was erased from a `Receiver<usize>`, we
    /// //  can downcast it to a `Sender<usize>`.
    /// let tx = rx.downcast_sender::<usize>().await.unwrap();
    /// # drop(tx)
    /// # }
    /// ```
    ///
    /// An error is returned when the erased type does not match the
    /// requested type:
    ///
    /// ```
    /// use tricky_pipe::oneshot;
    ///
    /// # #[tokio::main(flavor = "current_thread")] async fn main() {
    /// let rx = oneshot::Receiver::<usize>::new().into_erased();
    ///
    /// // Requesting a differently-typed sender, such as `&'static str`,
    /// // will return an error:
    /// let err = rx.downcast_sender::<&'static str>().await
    ///     .unwrap_err();
    /// assert_eq!(err, oneshot::ErasedSenderError::WrongType);
    /// # }
    pub async fn downcast_sender<T: Send + 'static>(&self) -> Result<Sender<T>, ErasedSenderError> {
        if TypeId::of::<T>() != self.type_id {
            return Err(ErasedSenderError::WrongType);
        }

        self.take_sender()
            .await
            .map_err(ErasedSenderError::Sender)?;
        unsafe { (self.vtable.clone)(self.chan) }
        Ok(Sender {
            chan: self.chan.cast(),
            vtable: self.vtable,
            sent: false,
        })
    }

    async fn take_sender(&self) -> Result<(), SenderError> {
        test_span!("Oneshot::sender");
        let this = unsafe { &*self.chan };
        while let Err(state) =
            test_dbg!(this
                .head
                .state
                .compare_exchange(HAS_RX, HAS_RX | HAS_TX, AcqRel, Acquire))
        {
            if state & SENT != 0 {
                let _ = test_dbg!(self.recv_erased().await.map(|_| ()));
            }

            if state & CLOSED != 0 {
                return Err(SenderError::Closed);
            }

            if state & HAS_TX != 0 {
                return Err(SenderError::SenderAlreadyActive);
            }
        }

        Ok(())
    }

    /// Await a message from a [`Sender`] or [`DeserSender`].
    ///
    /// If a sender has not been created, this function will immediately return
    /// [`RecvError::NoSender`]. If the sender is dropped without sending a
    /// response, this function will return [`RecvError::NoSender`] after the
    /// sender has been dropped.
    pub fn recv_erased(&self) -> RecvErased<'_> {
        RecvErased { rx: self }
    }

    /// Await a message from a [`Sender`] or [`DeserSender`].
    pub fn poll_recv_erased<'rx>(
        &'rx self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<ErasedRecvRef<'rx>, RecvError>> {
        let this = unsafe { &*self.chan };
        this.poll_recv(cx).map_ok(|_| ErasedRecvRef {
            rx: self,
            taken: false,
        })
    }

    /// Close the Oneshot. This will cause any pending senders to fail.
    pub fn close(&self) {
        unsafe {
            if (*self.chan).head.close() {
                (self.drop_data)(self.chan);
            }
        }
    }
}

impl fmt::Debug for ErasedReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("oneshot::ErasedReceiver")
            .field("chan", &format_args!("{:p}", self.chan))
            .finish()
    }
}

impl Drop for ErasedReceiver {
    fn drop(&mut self) {
        unsafe {
            // decrement the ref count, if this sender was constructed from an
            // `Arc<Oneshot>`.
            (self.vtable.drop)(self.chan)
        }
    }
}

unsafe impl Send for ErasedReceiver {}
unsafe impl Sync for ErasedReceiver {}

// === impl Header ===

impl Header {
    fn close(&self) -> bool {
        // Immediately mark the state as closed
        let state = test_dbg!(self.state.swap(CLOSED, AcqRel));
        // Mark the waiter as closed (shouldn't be necessary - you can only create
        // a waiter from the Reusable type, which we are now dropping).
        self.wait.close();

        if test_dbg!(state & SENT != 0) && test_dbg!(state & HAS_TX == 0) {
            // We have received a message, but are dropping before reception.
            // We are responsible to drop the contents.
            return true;
        }

        // This SHOULD be impossible, as closing requires dropping the
        // Oneshot. Make this a debug assert to catch if this ever happens
        // during development or testing, otherwise do nothing.
        debug_assert!(state & CLOSED == 0, "Oneshot already closed while closing?");

        false
    }

    // Attempt to swap back to READY. This COULD fail if we just swapped to closed,
    // but in that case we won't override the CLOSED state, and it becomes OUR
    // responsibility to drop the contents.
    fn finish_write(&self) -> Result<(), Closed<()>> {
        let state = self.state.fetch_or(SENT, AcqRel);

        test_println!("finish_write: state={state:#b}");

        if test_dbg!(state & CLOSED != 0) {
            return Err(Closed(()));
        }

        if test_dbg!(state & RX_WAITING != 0) {
            test_dbg!(self.wait.wake());
        }

        Ok(())
    }

    fn drop_tx(&self) {
        self.state.fetch_and(!HAS_TX, AcqRel);
    }
}

// === impl Recv ===

impl<T> Future for Recv<'_, T> {
    type Output = Result<T, RecvError>;
    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_recv(cx)
    }
}

// === impl ErasedRecv ===

impl<'rx> Future for RecvErased<'rx> {
    type Output = Result<ErasedRecvRef<'rx>, RecvError>;
    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_recv_erased(cx)
    }
}

// === impl ErasedRecvRef ===

impl ErasedRecvRef<'_> {
    /// Attempts to downcast the received value to a `T`-typed value.
    ///
    /// If the [`ErasedReceiver`] that returned this `ErasedRecvRef` holds a
    /// `T`-typed message, this method returns [`Ok`]`<T>`. Otherwise, this
    /// method returns [`Err`]`(Self)`, allowing the `ErasedRecvRef` to be
    /// downcast to a different type.
    pub fn downcast<T: Send + 'static>(mut self) -> Result<T, Self> {
        if TypeId::of::<T>() != self.rx.type_id {
            return Err(self);
        }

        self.taken = true;
        unsafe {
            let chan = self.rx.chan.cast::<Oneshot<T>>();
            Ok((*chan).take_value())
        }
    }
}

impl Drop for ErasedRecvRef<'_> {
    fn drop(&mut self) {
        if self.taken {
            return;
        }

        unsafe {
            (self.rx.drop_data)(self.rx.chan);
            test_dbg!((*self.rx.chan).head.state.store(HAS_RX, Release));
        }
    }
}

// === impl Sender ===

impl<T> Sender<T> {
    /// Consume the sender, providing it with a reply.
    pub fn send(mut self, item: T) -> Result<(), Closed<T>> {
        let chan = self.chan();
        chan.cell
            .with_mut(|cell| unsafe { cell.write(MaybeUninit::new(item)) });

        match chan.head.finish_write() {
            Err(_) => {
                // Yup, a close happened WHILE we were writing. Go ahead and
                // take back the contents.
                let item = chan
                    .cell
                    .with_mut(|cell| unsafe { core::ptr::read(cell.cast::<T>()) });
                Err(Closed(item))
            }
            Ok(_) => {
                self.sent = true;
                Ok(())
            }
        }
    }

    #[inline(always)]
    fn chan(&self) -> &Oneshot<T> {
        unsafe { &*self.chan }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if !self.sent {
            // Attempt to move the state from WAITING to IDLE, and wake any
            // pending waiters. This will cause an Err(()) on the receive side.
            self.chan().head.drop_tx();
        }
        unsafe {
            // decrement the ref count, if this sender was constructed from an
            // `Arc<Oneshot>`.
            (self.vtable.drop)(self.chan.cast())
        }
    }
}

impl<T> fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { vtable, sent, .. } = self;
        f.debug_struct("oneshot::Sender")
            // .field("chan", self.chan())
            .field("vtable", vtable)
            .field("sent", sent)
            .finish()
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Sync for Sender<T> {}

// === impl DeserSender ===

impl DeserSender {
    /// Attempts to deserialize a message from `bytes` and send it to the
    /// corresponding [`Receiver`].
    ///
    /// # Returns
    ///
    /// - `Ok(())` if a message was sent successfully
    /// - [`Err`]`(`[`DeserSendError::Deserialize]`(`[`postcard::Error``]`))`,
    ///   if the message could not be deserialized as the type expected by the
    ///   [`Receiver`].
    /// - [`Err`]`(`[`DeserSendError::Closed`]`)` if the [`Receiver::close`]
    ///   method has been called.
    pub fn send(self, bytes: &[u8]) -> Result<(), DeserSendError> {
        (self.ser_vtable.from_bytes)(&self, bytes).map_err(self.deser_error())?;

        self.finish_write()
    }

    /// Attempts to deserialize a COBS-framed message from `bytes` and send it to the
    /// corresponding [`Receiver`].
    ///
    /// # Returns
    ///
    /// - `Ok(())` if a message was sent successfully
    /// - [`Err`]`(`[`DeserSendError::Deserialize]`(`[`postcard::Error``]`))`,
    ///   if the message could not be deserialized as the type expected by the
    ///   [`Receiver`].
    /// - [`Err`]`(`[`DeserSendError::Closed`]`)` if the [`Receiver::close`]
    ///   method has been called.
    pub fn send_framed(self, bytes: &mut [u8]) -> Result<(), DeserSendError> {
        (self.ser_vtable.from_bytes_framed)(&self, bytes).map_err(self.deser_error())?;

        self.finish_write()
    }

    fn deser_error(&self) -> impl FnOnce(postcard::Error) -> DeserSendError {
        let info = self.ser_vtable.typeinfo;
        move |error| DeserSendError::Deserialize {
            error,
            message_type: info.name(),
        }
    }

    fn finish_write(mut self) -> Result<(), DeserSendError> {
        match self.head().finish_write() {
            Err(_) => {
                // Yup, a close happened WHILE we were writing. Go ahead and drop
                // the contents
                (self.ser_vtable.drop_data)(&self);
                Err(DeserSendError::Closed)
            }
            Ok(_) => {
                self.sent = true;
                Ok(())
            }
        }
    }

    fn head(&self) -> &Header {
        unsafe { &(*self.chan).head }
    }
}

impl Drop for DeserSender {
    fn drop(&mut self) {
        if !self.sent {
            // Attempt to move the state from WAITING to IDLE, and wake any
            // pending waiters. This will cause an Err(()) on the receive side.
            self.head().drop_tx();
        }

        unsafe {
            // decrement the ref count, if this sender was constructed from an
            // `Arc<Oneshot>`.
            (self.vtable.drop)(self.chan)
        }
    }
}

impl fmt::Debug for DeserSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            sent,
            vtable,
            ser_vtable,
            ..
        } = self;
        f.debug_struct("oneshot::DeserSender")
            .field("type", &ser_vtable.typeinfo)
            .field("head", self.head())
            .field("vtable", vtable)
            .field("sent", sent)
            .finish()
    }
}

// Safety: `DeserSender`s can only be constructed when the value in the channel
// is `Send`, so they are also `Send + Sync`.
unsafe impl Send for DeserSender {}

// Safety: `DeserSender`s can only be constructed when the value in the channel
// is `Send`, so they are also `Send + Sync`.
unsafe impl Sync for DeserSender {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loom::{self, future::block_on, thread};

    #[derive(Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct DeserStruct {
        hello: String,
        one: usize,
    }

    #[test]
    fn basically_works() {
        loom::model(|| {
            let rx = Receiver::new();
            let tx = block_on(rx.sender()).unwrap();

            thread::spawn(move || {
                test_dbg!(tx.send(1)).unwrap();
            });

            assert_eq!(test_dbg!(block_on(rx.recv())), Ok(1));
        });
    }

    #[test]
    fn recv_closed() {
        loom::model(|| {
            let rx = Receiver::new();
            let tx = block_on(rx.sender()).unwrap();

            thread::spawn(move || {
                let _ = test_dbg!(tx.send(1));
            });

            rx.close();
            let _ = test_dbg!(block_on(rx.recv()));
        });
    }

    #[test]
    fn reuse() {
        loom::model(|| {
            let rx = Receiver::new();

            let tx = block_on(rx.sender()).unwrap();
            thread::spawn(move || {
                block_on(async move {
                    test_dbg!(tx.send(1)).unwrap();
                });
            });

            assert_eq!(test_dbg!(block_on(rx.recv())), Ok(1));

            let tx = block_on(rx.sender()).unwrap();
            thread::spawn(move || {
                block_on(async move {
                    test_dbg!(tx.send(2)).unwrap();
                });
            });

            assert_eq!(test_dbg!(block_on(rx.recv())), Ok(2));
        });
    }

    #[test]
    fn erased_rx() {
        loom::model(|| {
            let rx = Receiver::<usize>::new().into_erased();

            for i in 1..2 {
                let tx_err = block_on(rx.downcast_sender::<&'static str>())
                    .expect_err("downcasting sender to the wrong type should fail");
                assert_eq!(ErasedSenderError::WrongType, tx_err);

                let tx = block_on(rx.downcast_sender::<usize>())
                    .expect("downcasting sender to the correct type should succeed");

                thread::spawn(move || {
                    block_on(async move {
                        test_dbg!(tx.send(i)).unwrap();
                    });
                });

                let recv =
                    test_dbg!(block_on(rx.recv_erased())).expect("recv_erased should succeed");
                let recv = recv
                    .downcast::<&'static str>()
                    .expect_err("downcasting ErasedRecvRef to the wrong type should fail");
                assert_eq!(
                    recv.downcast::<usize>()
                        .expect("downcasting to the correct type should succeed"),
                    i
                );
            }
        });
    }

    #[test]
    fn deserialize_tx() {
        loom::model(|| {
            let rx = Receiver::new();
            let tx = block_on(rx.deser_sender()).unwrap();

            let value = DeserStruct {
                hello: "hello".to_string(),
                one: 1,
            };
            let bytes = postcard::to_allocvec(&value).unwrap();

            thread::spawn(move || {
                test_dbg!(tx.send(&bytes[..])).unwrap();
            });

            assert_eq!(test_dbg!(block_on(rx.recv())), Ok(value));

            let tx = block_on(rx.deser_sender()).unwrap();
            let value2 = DeserStruct {
                hello: "world".to_string(),
                one: 100000,
            };
            let bytes = postcard::to_allocvec(&value2).unwrap();
            thread::spawn(move || {
                test_dbg!(tx.send(&bytes[..])).unwrap();
            });

            assert_eq!(test_dbg!(block_on(rx.recv())), Ok(value2));
        });
    }
}
