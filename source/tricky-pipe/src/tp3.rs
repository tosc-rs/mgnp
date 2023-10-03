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
use serde::Serialize;

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

#[cfg(feature = "alloc")]
type RecvVecFn = for<'pipe> fn(*const (), u8) -> Vec<u8>;
type RecvIntoFn = fn(*const (), u8, &mut [u8]) -> postcard::Result<&mut [u8]>;

impl<T> TrickyPipe<T> {
    const EMPTY_CELL: UnsafeCell<MaybeUninit<T>> = UnsafeCell::new(MaybeUninit::uninit());
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
            recv_into: Self::recv_into,

            #[cfg(feature = "alloc")]
            recv_vec: Self::recv_vec,
        })
    }

    fn recv_into(elems: *const (), idx: u8, buf: &mut [u8]) -> postcard::Result<&mut [u8]> {
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

    #[cfg(feature = "alloc")]
    fn recv_vec(elems: *const (), idx: u8, buf: &mut [u8]) -> postcard::Result<Vec<[u8]>> {
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
    #[cfg(feature = "alloc")]
    recv_vec: RecvVecFn,
    recv_into: RecvIntoFn,
}

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
    pub fn try_recv_into<'buf>(
        &self,
        buf: &'buf mut [u8],
    ) -> Result<&'buf mut [u8], SerTryRecvError> {
        let res = self.core.try_dequeue().ok_or(SerTryRecvError::Empty)?;
        (self.recv_into)(self.elems, res.idx, buf)
            .map_err(|e| SerTryRecvError::Recv(SerRecvError::Ser(e)))
    }

    pub async fn recv_into<'buf>(
        &self,
        buf: &'buf mut [u8],
    ) -> Result<&'buf mut [u8], SerRecvError> {
        let res = loop {
            match self.core.try_dequeue() {
                Some(res) => break res,
                None => self
                    .core
                    .cons_wait
                    .wait()
                    .await
                    .map_err(|_| SerRecvError::Closed)?,
            }
        };

        (self.recv_into)(self.elems, res.idx, buf).map_err(SerRecvError::Ser)
    }

    #[cfg(feature = "alloc")]
    pub fn try_recv(&self) -> Result<Vec<u8>, SerTryRecvError> {
        let res = self.core.try_dequeue().ok_or(SerTryRecvError::Empty)?;
        (self.recv_vec)(self.elems, res.idx)
            .map_err(|e| SerTryRecvError::Recv(SerRecvError::Ser(e)))
    }

    #[cfg(feature = "alloc")]
    pub async fn recv<'buf>(&self, buf: &'buf mut [u8]) -> Result<&'buf mut [u8], SerRecvError> {
        loop {
            match self.try_recv() {
                Ok(res) => return Ok(res),
                Err(SerTryRecvError::Empty) => self
                    .core
                    .cons_wait
                    .wait()
                    .await
                    .map_err(|_| SerRecvError::Closed)?,
                Err(SerTryRecvError::Recv(e)) => return Err(e),
            }
        }
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
            let seq = slot.load(Ordering::Acquire) >> SHIFT;
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
                        return Some(Reservation {
                            core: self,
                            idx: pos as u8,
                        });
                    }
                }
                dif if dif < 0 => return None,
                _ => pos = self.dequeue_pos.load(Ordering::Relaxed),
            }
        }
    }

    fn commit(&self, idx: u8) {
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
        self.commit();
    }

    pub fn commit(self) {
        // don't run the destructor that frees the index, since we are dropping
        // the cell...
        let pipe = ManuallyDrop::new(self.pipe);
        // ...and commit to the queue.
        pipe.core.commit(pipe.idx);
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

pub enum TryEnqueueError {
    Full,
    Closed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TryRecvError {
    Empty,
    Recv(RecvError),
}

pub enum RecvError {
    Closed,
}

pub enum SerTryRecvError {
    Empty,
    Recv(SerRecvError),
}

pub enum SerRecvError {
    Closed,
    Ser(postcard::Error),
}
