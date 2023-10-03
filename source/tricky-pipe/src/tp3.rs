use crate::loom::{
    cell::{self, UnsafeCell},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use core::{
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
};
use maitake_sync::{WaitCell, WaitQueue};
use mnemos_bitslab::index::IndexAllocWord;
use serde::{de::DeserializeOwned, Serialize};

const CAPACITY: usize = IndexAllocWord::CAPACITY as usize;
const MASK: usize = CAPACITY - 1;
const SHIFT: usize = MASK.count_ones() as usize;
const SEQ_ONE: usize = 1 << SHIFT;

pub struct TrickyPipe<T> {
    elements: [UnsafeCell<MaybeUninit<T>>; CAPACITY],
    core: Core,
}

struct Core {
    dequeue_pos: AtomicUsize,
    enqueue_pos: AtomicUsize,
    cons_wait: WaitCell,
    prod_wait: WaitQueue,
    indices: IndexAllocWord,
    queue: [AtomicUsize; CAPACITY],
    rx_claimed: AtomicBool,
}

impl<T> TrickyPipe<T> {
    const EMPTY_CELL: UnsafeCell<MaybeUninit<T>> = UnsafeCell::new(MaybeUninit::uninit());
    #[allow(clippy::declare_interior_mutable_const)]
    const QUEUE_INIT: AtomicUsize = AtomicUsize::new(0);

    pub const fn new() -> Self {
        Self {
            core: Core {
                dequeue_pos: AtomicUsize::new(0),
                enqueue_pos: AtomicUsize::new(0),
                cons_wait: WaitCell::new(),
                prod_wait: WaitQueue::new(),
                indices: IndexAllocWord::new(),
                queue: [Self::QUEUE_INIT; CAPACITY],
                rx_claimed: AtomicBool::new(false),
            },
            elements: [Self::EMPTY_CELL; CAPACITY],
        }
    }

    pub fn receiver(&self) -> Option<Receiver<'_, T>> {
        self.core
            .rx_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;

        Some(Receiver { pipe: self })
    }

    pub fn sender(&self) -> Sender<'_, T> {
        Sender { pipe: self }
    }
}

impl<T: Serialize> TrickyPipe<T> {
    pub fn ser_receiver(&self) -> Option<SerReceiver<'_>> {
        self.core
            .rx_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;

        Some(SerReceiver {
            core: &self.core,
            elems: self.elements.as_ptr() as *const (),
            vtable: Self::SER_VTABLE,
        })
    }

    const SER_VTABLE: &'static SerVtable = &SerVtable {
        #[cfg(feature = "alloc")]
        to_vec: Self::to_vec,
        #[cfg(feature = "alloc")]
        to_vec_framed: Self::to_vec_framed,
        to_slice: Self::to_slice,
        to_slice_framed: Self::to_slice_framed,
    };

    fn to_slice(elems: *const (), idx: u8, buf: &mut [u8]) -> postcard::Result<&mut [u8]> {
        unsafe {
            let elems = elems as *const UnsafeCell<MaybeUninit<T>>;
            // TODO(eliza): since this is unsafe anyway, we *could* just do
            // pointer math and elide the bounds check... &shrug;
            let elems = core::slice::from_raw_parts(elems, CAPACITY);
            elems[idx as usize].with(|ptr| {
                let elem = (*ptr).assume_init_ref();
                postcard::to_slice(elem, buf)
            })
        }
    }

    fn to_slice_framed(elems: *const (), idx: u8, buf: &mut [u8]) -> postcard::Result<&mut [u8]> {
        unsafe {
            let elems = elems as *const UnsafeCell<MaybeUninit<T>>;
            // TODO(eliza): since this is unsafe anyway, we *could* just do
            // pointer math and elide the bounds check... &shrug;
            let elems = core::slice::from_raw_parts(elems, CAPACITY);
            elems[idx as usize].with(|ptr| {
                let elem = (*ptr).assume_init_ref();
                postcard::to_slice_cobs(elem, buf)
            })
        }
    }

    #[cfg(feature = "alloc")]
    fn to_vec(elems: *const (), idx: u8, buf: &mut [u8]) -> postcard::Result<Vec<[u8]>> {
        unsafe {
            let elems = elems as *const UnsafeCell<MaybeUninit<T>>;
            // TODO(eliza): since this is unsafe anyway, we *could* just do
            // pointer math and elide the bounds check... &shrug;
            let elems = core::slice::from_raw_parts(elems, CAPACITY);
            elems[idx as usize].with(|ptr| {
                let elem = (*ptr).assume_init_ref();
                postcard::to_allocvec(elem, buf)
            })
        }
    }

    #[cfg(feature = "alloc")]
    fn to_vec_framed(elems: *const (), idx: u8, buf: &mut [u8]) -> postcard::Result<Vec<[u8]>> {
        unsafe {
            let elems = elems as *const UnsafeCell<MaybeUninit<T>>;
            // TODO(eliza): since this is unsafe anyway, we *could* just do
            // pointer math and elide the bounds check... &shrug;
            let elems = core::slice::from_raw_parts(elems, CAPACITY);
            elems[idx as usize].with(|ptr| {
                let elem = (*ptr).assume_init_ref();
                postcard::to_allocvec_cobs(elem, buf)
            })
        }
    }
}

impl<T: DeserializeOwned> TrickyPipe<T> {
    pub fn ser_sender(&self) -> SerSender<'_> {
        SerSender {
            core: &self.core,
            elems: self.elements.as_ptr() as *const (),
            vtable: Self::DESER_VTABLE,
        }
    }

    const DESER_VTABLE: &'static DeserVtable = &DeserVtable {
        from_bytes: Self::from_bytes,
        from_bytes_framed: Self::from_bytes_framed,
    };

    fn from_bytes(elems: *const (), idx: u8, buf: &[u8]) -> postcard::Result<()> {
        let val = postcard::from_bytes(buf)?;
        unsafe {
            let elems = elems as *const UnsafeCell<MaybeUninit<T>>;
            // TODO(eliza): since this is unsafe anyway, we *could* just do
            // pointer math and elide the bounds check... &shrug;
            let elems = core::slice::from_raw_parts(elems, CAPACITY);
            elems[idx as usize].with_mut(|ptr| (*ptr).write(val));
        }
        Ok(())
    }

    fn from_bytes_framed(elems: *const (), idx: u8, buf: &[u8]) -> postcard::Result<()> {
        let val = postcard::from_bytes(buf)?;
        unsafe {
            let elems = elems as *const UnsafeCell<MaybeUninit<T>>;
            // TODO(eliza): since this is unsafe anyway, we *could* just do
            // pointer math and elide the bounds check... &shrug;
            let elems = core::slice::from_raw_parts(elems, CAPACITY);
            elems[idx as usize].with_mut(|ptr| (*ptr).write(val));
        }
        Ok(())
    }
}

pub struct Receiver<'pipe, T> {
    pipe: &'pipe TrickyPipe<T>,
}

pub struct Sender<'pipe, T> {
    pipe: &'pipe TrickyPipe<T>,
}

pub struct SerReceiver<'pipe> {
    core: &'pipe Core,
    elems: *const (),
    vtable: &'static SerVtable,
}

pub struct SerSender<'pipe> {
    core: &'pipe Core,
    elems: *const (),
    vtable: &'static DeserVtable,
}

pub struct SerRecvRef<'pipe> {
    pipe: Reservation<'pipe>,
    elems: *const (),
    vtable: &'static SerVtable,
}

struct SerVtable {
    #[cfg(feature = "alloc")]
    to_vec: SerVecFn,
    #[cfg(feature = "alloc")]
    to_vec_framed: SerVecFn,
    to_slice: SerFn,
    to_slice_framed: SerFn,
}

struct DeserVtable {
    from_bytes: DeserFn,
    from_bytes_framed: DeserFn,
}

type SerFn = fn(*const (), u8, &mut [u8]) -> postcard::Result<&mut [u8]>;

#[cfg(feature = "alloc")]
type SerVecFn = fn(*const (), u8) -> postcard::Result<Vec<u8>>;

type DeserFn = fn(*const (), u8, &[u8]) -> postcard::Result<()>;

// === impl Receiver ===

impl<T> Receiver<'_, T> {
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let res = self.pipe.core.try_dequeue().ok_or(TryRecvError::Empty)?;
        let elem =
            self.pipe.elements[res.idx as usize].with(|ptr| unsafe { (*ptr).assume_init_read() });
        Ok(elem)
    }

    pub async fn recv(&self) -> Result<T, RecvError> {
        loop {
            match self.try_recv() {
                Ok(e) => return Ok(e),
                Err(TryRecvError::Empty) => self
                    .pipe
                    .core
                    .cons_wait
                    .wait()
                    .await
                    .map_err(|_| RecvError::Closed)?,
                Err(TryRecvError::Recv(e)) => return Err(e),
            }
        }
    }
}

impl SerReceiver<'_> {
    /// Attempt to receive a message from the channel, if there are currently
    /// any messages in the channel.
    ///
    /// This method returns a [`SerRecvRef`] which may be used to serialize the
    /// message.
    pub fn try_recv(&self) -> Result<SerRecvRef<'_>, TryRecvError> {
        let res = self.core.try_dequeue().ok_or(TryRecvError::Empty)?;
        Ok(SerRecvRef {
            pipe: res,
            elems: self.elems,
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
            match self.core.try_dequeue() {
                Some(res) => break res,
                None => self
                    .core
                    .cons_wait
                    .wait()
                    .await
                    .map_err(|_| RecvError::Closed)?,
            }
        };

        Ok(SerRecvRef {
            pipe: res,
            elems: self.elems,
            vtable: self.vtable,
        })
    }
}

impl SerRecvRef<'_> {
    pub fn to_slice<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        (self.vtable.to_slice)(self.elems, self.pipe.idx, buf)
    }

    pub fn to_slice_framed<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        (self.vtable.to_slice_framed)(self.elems, self.pipe.idx, buf)
    }

    /// Serializes the message to an owned `Vec`.
    #[cfg(feature = "alloc")]
    pub fn to_vec(&self) -> postcard::Result<alloc::vec::Vec<u8>> {
        (self.vtable.to_vec)(self.elems, self.pipe.idx)
    }

    /// Returns the serialized representation of the message as a COBS frame, in
    /// an owned `Vec`.
    #[cfg(feature = "alloc")]
    pub fn to_vec_framed(&self) -> postcard::Result<alloc::vec::Vec<u8>> {
        (self.vtable.to_vec_framed)(self.elems, self.pipe.idx)
    }
}

impl SerSender<'_> {
    pub fn try_send(&self, bytes: &[u8]) -> Result<(), SerTrySendError> {
        self.try_send_inner(bytes, self.vtable.from_bytes_framed)
    }

    pub fn try_send_framed(&self, bytes: &[u8]) -> Result<(), SerTrySendError> {
        self.try_send_inner(bytes, self.vtable.from_bytes_framed)
    }

    pub async fn send(&self, bytes: &[u8]) -> Result<(), SerSendError> {
        self.send_inner(bytes, self.vtable.from_bytes).await
    }

    pub async fn send_framed(&self, bytes: &[u8]) -> Result<(), SerSendError> {
        self.send_inner(bytes, self.vtable.from_bytes_framed).await
    }

    async fn send_inner(&self, bytes: &[u8], deserialize: DeserFn) -> Result<(), SerSendError> {
        loop {
            match self.core.try_reserve() {
                Some(res) => {
                    // try writing the bytes to the reservation.
                    deserialize(self.elems, res.idx, bytes).map_err(SerSendError::Deserialize)?;
                    // if we successfully deserialized the bytes, commit the send.
                    // otherwise, we'll release the send index when we drop the reservation.
                    res.commit_send();
                    return Ok(());
                }
                None => self
                    .core
                    .prod_wait
                    .wait()
                    .await
                    .map_err(|_| SerSendError::Closed)?,
            }
        }
    }

    fn try_send_inner(&self, bytes: &[u8], deserialize: DeserFn) -> Result<(), SerTrySendError> {
        let res = self.core.try_reserve().ok_or(SerTrySendError::Full)?;
        // try writing the bytes to the reservation.
        deserialize(self.elems, res.idx, bytes)
            .map_err(|err| SerTrySendError::Send(SerSendError::Deserialize(err)))?;
        // if we successfully deserialized the bytes, commit the send.
        // otherwise, we'll release the send index when we drop the reservation.
        res.commit_send();
        Ok(())
    }
}

impl<T> Sender<'_, T> {
    pub fn try_reserve(&self) -> Result<SendRef<'_, T>, TryEnqueueError> {
        let pipe = self.pipe.core.try_reserve().ok_or(TryEnqueueError::Full)?;
        let cell = self.pipe.elements[pipe.idx as usize].get_mut();
        Ok(SendRef { cell, pipe })
    }

    pub async fn reserve(&self) -> Result<SendRef<'_, T>, EnqueueError> {
        let pipe = self.pipe.core.reserve().await?;
        let cell = self.pipe.elements[pipe.idx as usize].get_mut();
        Ok(SendRef { cell, pipe })
    }
}

impl Core {
    fn try_reserve(&self) -> Option<Reservation<'_>> {
        self.indices
            .allocate()
            .map(|idx| Reservation { core: self, idx })
    }

    async fn reserve(&self) -> Result<Reservation, EnqueueError> {
        loop {
            match self.try_reserve() {
                Some(res) => return Ok(res),
                None => self
                    .prod_wait
                    .wait()
                    .await
                    .map_err(|_| EnqueueError::Closed)?,
            }
        }
    }

    fn try_dequeue(&self) -> Option<Reservation<'_>> {
        let mut pos = self.dequeue_pos.load(Ordering::Relaxed);
        loop {
            let slot = &self.queue[pos & MASK];
            let val = slot.load(Ordering::Acquire);
            let seq = val >> SHIFT;
            let dif = (seq as i8).wrapping_sub(pos as i8);

            match dif {
                0 => {
                    if self
                        .dequeue_pos
                        .compare_exchange_weak(
                            pos,
                            pos.wrapping_add(1),
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        slot.store(val.wrapping_add(SEQ_ONE), Ordering::Release);
                        return Some(Reservation {
                            core: self,
                            idx: (val & MASK) as u8,
                        });
                    }
                }
                dif if dif < 0 => return None,
                _ => pos = self.dequeue_pos.load(Ordering::Relaxed),
            }
        }
    }

    fn commit_send(&self, idx: u8) {
        debug_assert!(idx as usize <= MASK);
        let mut pos = self.enqueue_pos.load(Ordering::Relaxed);
        loop {
            let slot = &self.queue[pos & MASK];
            let seq = slot.load(Ordering::Acquire) >> SHIFT;
            let dif = (seq as i8).wrapping_sub(pos as i8);

            match dif {
                0 => {
                    if self
                        .enqueue_pos
                        .compare_exchange_weak(
                            pos,
                            pos.wrapping_add(1),
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        let new = (pos << SHIFT).wrapping_add(SEQ_ONE);
                        slot.store(idx as usize | new, Ordering::Release);
                        self.cons_wait.wake();
                        return;
                    }
                }
                dif if dif < 0 => unreachable!(),
                _ => pos = self.enqueue_pos.load(Ordering::Relaxed),
            }
        }
    }

    unsafe fn uncommit(&self, idx: u8) {
        self.indices.free(idx);
        self.prod_wait.wake();
    }
}

pub struct SendRef<'core, T> {
    // load bearing drop ordering lol lmao
    cell: cell::MutPtr<MaybeUninit<T>>,
    pipe: Reservation<'core>,
}

struct Reservation<'core> {
    core: &'core Core,
    idx: u8,
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

impl Reservation<'_> {
    fn commit_send(self) {
        // don't run the destructor that frees the index, since we are dropping
        // the cell...
        let this = ManuallyDrop::new(self);
        // ...and commit to the queue.
        this.core.commit_send(this.idx);
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        unsafe { self.core.uncommit(self.idx) }
    }
}

/// Represents a closed error
#[derive(Debug, Eq, PartialEq)]
pub enum EnqueueError {
    Closed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TryEnqueueError {
    Full,
    Closed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TryRecvError {
    Empty,
    Recv(RecvError),
}

#[derive(Debug, Eq, PartialEq)]
pub enum RecvError {
    Closed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum SerSendError {
    Closed,
    Deserialize(postcard::Error),
}

#[derive(Debug, Eq, PartialEq)]
pub enum SerTrySendError {
    Full,
    Send(SerSendError),
}
