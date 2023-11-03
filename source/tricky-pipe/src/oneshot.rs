//! One-Shot Channels
use crate::{
    loom::{
        cell::UnsafeCell,
        hint,
        sync::atomic::{AtomicU8, Ordering::*},
    },
    typeinfo::TypeInfo,
};
use core::{
    future::Future,
    mem::MaybeUninit,
    pin::Pin,
    task::{self, Context, Poll},
};
use serde::de::DeserializeOwned;

use maitake_sync::WaitCell;

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
/// given time, and a response can be received through a call to
/// [Reusable::receive].
#[repr(C)]
pub struct Oneshot<T> {
    head: Header,
    cell: UnsafeCell<MaybeUninit<T>>,
}

/// A single-use One-Shot channel sender
///
/// It can be consumed to send a response back to the [Reusable] instance that created
/// the [Sender].
pub struct Sender<T> {
    chan: *const Oneshot<T>,
    drop: unsafe fn(*const Oneshot<T>),
    sent: bool,
}

pub struct DeserSender {
    chan: *const Oneshot<()>,
    drop: unsafe fn(*const Oneshot<()>),
    vtable: &'static DeserVtable,
    sent: bool,
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
}

#[derive(Debug, Eq, PartialEq)]
pub enum DeserSendError {
    Deserialize(postcard::Error),
    Oneshot(OneshotError),
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

/// Not waiting for anything.
const IDLE: u8 = 0;
/// A Sender has been created, but no writes have begun yet
const HAS_TX: u8 = 0b0001;
/// The receiver is waiting for a value.
const RX_WAITING: u8 = 0b0010;
// A value has been sent.
const SENT: u8 = 0b0100;
/// The Oneshot has been manually closed or dropped.
const CLOSED: u8 = 0b1000;

impl<T> Oneshot<T> {
    /// Returns a new `Oneshot` channel.
    #[must_use]
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        Self {
            head: Header {
                state: AtomicU8::new(IDLE),
                wait: WaitCell::new(),
            },

            cell: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Returns a new `Oneshot` channel.
    #[must_use]
    #[cfg(loom)]
    pub fn new() -> Self {
        Self {
            head: Header {
                state: AtomicU8::new(IDLE),
                wait: WaitCell::new(),
            },

            cell: UnsafeCell::new(MaybeUninit::uninit()),
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
            sent: false,
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
            sent: false,
        })
    }

    async fn take_sender(&self) -> Result<(), OneshotError> {
        test_span!("Oneshot::take_sender");
        while let Err(state) = test_dbg!(self
            .head
            .state
            .compare_exchange(IDLE, HAS_TX, AcqRel, Acquire))
        {
            if state & SENT != 0 {
                let _ = test_dbg!(self.recv().await.map(|_| ()));
            }

            if state & CLOSED != 0 {
                return Err(OneshotError::ChannelClosed);
            }

            if state & HAS_TX != 0 {
                return Err(OneshotError::SenderAlreadyActive);
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
        test_span!("Oneshot::poll_recv");
        loop {
            let state = test_dbg!(self.head.state.fetch_or(RX_WAITING, AcqRel));

            if test_dbg!(state & CLOSED != 0) {
                return Poll::Ready(Err(OneshotError::ChannelClosed));
            }

            if test_dbg!(state & SENT != 0) {
                let mut ret = MaybeUninit::<T>::uninit();
                unsafe {
                    self.cell.with_mut(|cell| {
                        core::ptr::copy_nonoverlapping(cell.cast(), ret.as_mut_ptr(), 1);
                    });

                    test_dbg!(self.head.state.store(IDLE, Release));
                    return Poll::Ready(Ok(ret.assume_init()));
                }
            }

            if test_dbg!(state & HAS_TX == 0) {
                return Poll::Ready(Err(OneshotError::NoSenderActive));
            }

            // We are still waiting for the Sender to start or complete.
            // Trigger another wait cycle.
            task::ready!(test_dbg!(self.head.wait.poll_wait(cx)))
                .map_err(|_| OneshotError::ChannelClosed)?;
            hint::spin_loop();
        }
    }

    /// Close the Oneshot. This will cause any pending senders to fail.
    pub fn close(&self) {
        if self.head.close() {
            self.cell
                .with_mut(|cell| unsafe { core::ptr::drop_in_place(cell.cast::<T>()) });
        }
    }
}

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
    fn finish_write(&self) -> Result<(), OneshotError> {
        let state = self.state.fetch_or(SENT, AcqRel);

        test_println!("finish_write: state={state:#b}");

        if test_dbg!(state & CLOSED != 0) {
            return Err(OneshotError::ChannelClosed);
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

impl<T> Oneshot<T>
where
    T: DeserializeOwned + Send + 'static,
{
    const VTABLE: DeserVtable = DeserVtable {
        drop_data: |this| unsafe {
            this.vtable
                .typeinfo
                .assert_matches::<T>("oneshot::DeserSender");
            let chan = this.chan.cast::<Oneshot<T>>();
            (*chan).cell.with_mut(|cell| {
                core::ptr::drop_in_place(cell.cast::<T>());
            });
        },
        from_bytes: |this, bytes| -> postcard::Result<()> {
            this.vtable
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
            this.vtable
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

    #[cfg(any(test, feature = "alloc"))]
    pub async fn deser_sender(self: &Arc<Self>) -> Result<DeserSender, OneshotError> {
        self.take_sender().await?;
        Ok(DeserSender {
            chan: Arc::into_raw(self.clone()).cast(),
            drop: Arc::decrement_strong_count,
            vtable: &Self::VTABLE,
            sent: false,
        })
    }

    pub async fn deser_static_sender(&'static self) -> Result<DeserSender, OneshotError> {
        self.take_sender().await?;
        Ok(DeserSender {
            chan: self as *const _ as *const Oneshot<()>,
            drop: |_| {},
            vtable: &Self::VTABLE,
            sent: false,
        })
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
    pub fn send(mut self, item: T) -> Result<(), OneshotError> {
        let chan = unsafe { &*self.chan };

        chan.cell
            .with_mut(|cell| unsafe { cell.write(MaybeUninit::new(item)) });

        let finished = chan.head.finish_write();

        if let Err(OneshotError::ChannelClosed) = finished {
            // Yup, a close happened WHILE we were writing. Go ahead and drop the contents
            chan.cell.with_mut(|cell| unsafe {
                core::ptr::drop_in_place(cell.cast::<T>());
            });
        } else {
            self.sent = true;
        }
        finished
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        unsafe {
            if !self.sent {
                // Attempt to move the state from WAITING to IDLE, and wake any
                // pending waiters. This will cause an Err(()) on the receive side.
                (*self.chan).head.drop_tx();
            }
            // decrement the ref count, if this sender was constructed from an
            // `Arc<Oneshot>`.
            (self.drop)(self.chan)
        }
    }
}

unsafe impl<T: Send> Send for Sender<T> {}
unsafe impl<T: Send> Sync for Sender<T> {}

// === impl DeserSender ===

impl DeserSender {
    pub fn send(mut self, bytes: &[u8]) -> Result<(), DeserSendError> {
        let chan = unsafe { &*self.chan };

        (self.vtable.from_bytes)(&self, bytes).map_err(DeserSendError::Deserialize)?;

        let finished = chan.head.finish_write();

        if let Err(OneshotError::ChannelClosed) = finished {
            // Yup, a close happened WHILE we were writing. Go ahead and drop
            // the contents
            (self.vtable.drop_data)(&self)
        } else {
            self.sent = true;
        }

        finished.map_err(DeserSendError::Oneshot)
    }

    pub fn send_framed(mut self, bytes: &mut [u8]) -> Result<(), DeserSendError> {
        let chan = unsafe { &*self.chan };

        (self.vtable.from_bytes_framed)(&self, bytes).map_err(DeserSendError::Deserialize)?;

        let finished = chan.head.finish_write();

        if let Err(OneshotError::ChannelClosed) = finished {
            // Yup, a close happened WHILE we were writing. Go ahead and drop
            // the contents
            (self.vtable.drop_data)(&self)
        } else {
            self.sent = true;
        }

        finished.map_err(DeserSendError::Oneshot)
    }
}

impl Drop for DeserSender {
    fn drop(&mut self) {
        unsafe {
            if !self.sent {
                // Attempt to move the state from WAITING to IDLE, and wake any
                // pending waiters. This will cause an Err(()) on the receive side.
                (*self.chan).head.drop_tx();
            }

            // decrement the ref count, if this sender was constructed from an
            // `Arc<Oneshot>`.
            (self.drop)(self.chan)
        }
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
    use std::sync::Arc;

    #[derive(Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct DeserStruct {
        hello: String,
        one: usize,
    }

    #[test]
    fn basically_works() {
        loom::model(|| {
            let rx = Arc::new(Oneshot::new());
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
            let rx = Arc::new(Oneshot::new());
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
            let rx = Arc::new(Oneshot::new());

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
    fn deserialize_tx() {
        loom::model(|| {
            let rx = Arc::new(Oneshot::new());
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
