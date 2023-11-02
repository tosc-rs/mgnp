//! One-Shot Channels
use crate::loom::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU8, Ordering},
};
use core::{
    future::Future,
    mem::MaybeUninit,
    pin::Pin,
    task::{self, Context, Poll},
};

use maitake_sync::{Closed, WaitCell};

#[cfg(any(test, feature = "alloc"))]
use alloc::sync::Arc;

/// A reusable One-Shot channel.
///
/// Essentially, a Reusable is a single producer, single consumer, channel, with a max
/// depth of one. Many producers can be created over the lifecycle of a single consumer,
/// however only zero or one producers can be live at any given time.
///
/// A `Reusable<T>` can be used to hand out single-use [Sender] items, which can
/// be used to make a single reply.
///
/// A given `Reusable<T>` can only ever have zero or one `Sender<T>`s live at any
/// given time, and a response can be received through a call to [Reusable::receive].
pub struct Oneshot<T> {
    state: AtomicU8,
    cell: UnsafeCell<MaybeUninit<T>>,
    wait: WaitCell,
}

/// A single-use One-Shot channel sender
///
/// It can be consumed to send a response back to the [Reusable] instance that created
/// the [Sender].
pub struct Sender<T> {
    chan: *const Oneshot<T>,
    drop: unsafe fn(*const Oneshot<T>),
}

/// Future returned by [`Oneshot::recv`].
pub struct Recv<'rx, T> {
    rx: &'rx Oneshot<T>,
}

// An error type for the Reusable channel and Sender
#[derive(Debug, Eq, PartialEq)]
pub enum OneshotError {
    SenderAlreadyActive,
    NoSenderActive,
    ChannelClosed,
    InternalError,
}

impl From<Closed> for OneshotError {
    fn from(_: Closed) -> Self {
        OneshotError::ChannelClosed
    }
}

/// Not waiting for anything.
const IDLE: u8 = 0;
/// A Sender has been created, but no writes have begun yet
const WAITING: u8 = 1;
/// A Sender has begun writing, and will be dropped shortly.
const WRITING: u8 = 2;
/// The Sender has been dropped and the message has been sent
const READY: u8 = 3;
/// Reading has already started
const READING: u8 = 4;
/// The Oneshot has been manually closed or dropped.
const CLOSED: u8 = 5;

impl<T> Oneshot<T> {
    /// Returns a new `Oneshot` channel.
    #[must_use]
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(IDLE),
            cell: UnsafeCell::new(MaybeUninit::uninit()),
            wait: WaitCell::new(),
        }
    }

    /// Returns a new `Oneshot` channel.
    #[must_use]
    #[cfg(loom)]
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(IDLE),
            cell: UnsafeCell::new(MaybeUninit::uninit()),
            wait: WaitCell::new(),
        }
    }

    /// Create a sender for the given `Reusable<T>`. If a sender is already
    /// active, or the previous response has not yet been retrieved, an
    /// error will be immediately returned.
    ///
    /// This error can be cleared by awaiting [Reusable::receive].
    #[cfg(any(test, feature = "alloc"))]
    pub async fn sender(self: &Arc<Self>) -> Result<Sender<T>, OneshotError> {
        self.take_sender().await?;
        Ok(Sender {
            chan: Arc::into_raw(self.clone()),
            drop: Arc::decrement_strong_count,
        })
    }

    /// Create a sender for the given `Reusable<T>`. If a sender is already
    /// active, or the previous response has not yet been retrieved, an
    /// error will be immediately returned.
    ///
    /// This error can be cleared by awaiting [Reusable::receive].
    pub async fn static_sender(&'static self) -> Result<Sender<T>, OneshotError> {
        self.take_sender().await?;
        Ok(Sender {
            chan: self as *const Self,
            drop: |_| {},
        })
    }

    async fn take_sender(&self) -> Result<(), OneshotError> {
        while let Err(state) =
            self.state
                .compare_exchange(IDLE, WAITING, Ordering::AcqRel, Ordering::Relaxed)
        {
            match state {
                READY => {
                    let _ = self.recv().await;
                }
                WAITING | WRITING => return Err(OneshotError::SenderAlreadyActive),
                _ => return Err(OneshotError::InternalError),
            }
        }

        Ok(())
    }

    /// Await the response from a created sender.
    ///
    /// If a sender has not been created, this function will immediately return
    /// an error.
    ///
    /// If the sender is dropped without sending a response, this function will
    /// return an error after the sender has been dropped.
    pub fn recv(&self) -> Recv<'_, T> {
        Recv { rx: self }
    }

    /// Poll to receive a value from a [`Sender`].
    pub fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<Result<T, OneshotError>> {
        loop {
            let state =
                self.state
                    .compare_exchange(READY, READING, Ordering::AcqRel, Ordering::Relaxed);

            match state {
                Ok(_) => {
                    // We just swapped from READY to READING, that's a success!

                    let mut ret = MaybeUninit::<T>::uninit();
                    unsafe {
                        self.cell.with_mut(|cell| {
                            core::ptr::copy_nonoverlapping(cell.cast(), ret.as_mut_ptr(), 1);
                        });

                        self.state.store(IDLE, Ordering::Release);
                        return Poll::Ready(Ok(ret.assume_init()));
                    }
                }
                Err(WAITING | WRITING) => {
                    // We are still waiting for the Sender to start or complete.
                    // Trigger another wait cycle.
                    //
                    // NOTE: it's impossible for the wait to fail here, as we only
                    // close the channel when dropping the Reusable, which can't be
                    // done while the borrow of self is active in this function.
                    let _ = task::ready!(self.wait.poll_wait(cx));
                }
                Err(IDLE) => return Poll::Ready(Err(OneshotError::NoSenderActive)),
                Err(state) => unreachable!("invalid state {state:?}"),
            }
        }
    }

    /// Close the Oneshot. This will cause any pending senders to fail.
    pub fn close(self) {
        // Immediately mark the state as closed
        let old = self.state.swap(CLOSED, Ordering::AcqRel);
        // Mark the waiter as closed (shouldn't be necessary - you can only create
        // a waiter from the Reusable type, which we are now dropping).
        self.wait.close();

        // Determine if we need to drop the payload, if there is one.
        match old {
            IDLE => {
                // Nothing to do, already idle, no contents
            }
            WAITING => {
                // We are waiting for the sender, but it will fail to send.
                // Nothing to do.
            }
            WRITING => {
                // We are cancelling mid-send. This will cause the sender
                // to fail, and IT is responsible for dropping the almost-
                // sent message.
            }
            READY => {
                // We have received a message, but are dropping before reception.
                // We are responsible to drop the contents.
                self.cell
                    .with_mut(|cell| unsafe { core::ptr::drop_in_place(cell.cast::<T>()) });
            }
            READING => {
                // This SHOULD be impossible, as this is a transient state while
                // receiving, which shouldn't be possible if we are dropping the
                // Oneshot. Make this a debug assert to catch if this ever happens
                // during development or testing, otherwise do nothing.
                debug_assert!(false, "Dropped Oneshot while reading?");
            }
            CLOSED => {
                // This SHOULD be impossible, as closing requires dropping the
                // Oneshot. Make this a debug assert to catch if this ever happens
                // during development or testing, otherwise do nothing.
                debug_assert!(false, "Oneshot already closed while closing?");
            }
            _ => {}
        }
    }
}

unsafe impl<T: Send> Send for Oneshot<T> {}
unsafe impl<T: Send> Sync for Oneshot<T> {}

// === impl Recv ===

impl<T> Future for Recv<'_, T> {
    type Output = Result<T, OneshotError>;
    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.rx.poll_recv(cx)
    }
}

// === impl Sender ===

impl<T> Sender<T> {
    /// Consume the sender, providing it with a reply.
    pub fn send(self, item: T) -> Result<(), OneshotError> {
        let chan = unsafe { &*self.chan };
        let swap =
            chan.state
                .compare_exchange(WAITING, WRITING, Ordering::AcqRel, Ordering::Relaxed);

        match swap {
            Ok(_) => {}
            Err(CLOSED) => return Err(OneshotError::ChannelClosed),
            Err(_) => return Err(OneshotError::InternalError),
        };

        chan.cell
            .with_mut(|cell| unsafe { cell.write(MaybeUninit::new(item)) });

        // Attempt to swap back to READY. This COULD fail if we just swapped to closed,
        // but in that case we won't override the CLOSED state, and it becomes OUR
        // responsibility to drop the contents.
        let swap = chan
            .state
            .compare_exchange(WRITING, READY, Ordering::AcqRel, Ordering::Relaxed);

        match swap {
            Ok(_) => {}
            Err(CLOSED) => {
                // Yup, a close happened WHILE we were writing. Go ahead and drop the contents
                chan.cell.with_mut(|cell| unsafe {
                    core::ptr::drop_in_place(cell.cast::<T>());
                });
                return Err(OneshotError::ChannelClosed);
            }
            Err(_) => return Err(OneshotError::InternalError),
        }

        chan.wait.wake();
        Ok(())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        // Attempt to move the state from WAITING to IDLE, and wake any
        // pending waiters. This will cause an Err(()) on the receive side.
        {
            let chan = unsafe { &*self.chan };
            let _ = chan
                .state
                .compare_exchange(WAITING, IDLE, Ordering::AcqRel, Ordering::Relaxed);
            chan.wait.wake();
        }
        unsafe {
            // decrement the ref count, if this sender was constructed from an
            // `Arc<Oneshot>`.
            (self.drop)(self.chan)
        }
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Sync for Sender<T> {}
