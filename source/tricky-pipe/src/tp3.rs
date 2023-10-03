use crate::loom::{
    cell::{self, UnsafeCell},
    sync::atomic::{AtomicUsize, Ordering},
};
use core::{
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
};
use maitake_sync::{WaitCell, WaitQueue};
use mnemos_bitslab::index::IndexAllocWord;

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
}

impl<T> TrickyPipe<T> {
    const EMPTY_CELL: UnsafeCell<MaybeUninit<T>> = UnsafeCell::new(MaybeUninit::uninit());
    const QUEUE_INIT: AtomicUsize = AtomicUsize::new(0);

    const fn new() -> Self {
        Self {
            core: Core {
                dequeue_pos: AtomicUsize::new(0),
                enqueue_pos: AtomicUsize::new(0),
                cons_wait: WaitCell::new(),
                prod_wait: WaitQueue::new(),
                indices: IndexAllocWord::new(),
                queue: [Self::QUEUE_INIT; CAPACITY],
            },
            elements: [Self::EMPTY_CELL; CAPACITY],
        }
    }

    async fn reserve(&self) -> Result<SendRef<'_, T>, EnqueueError> {
        let pipe = self.core.reserve().await?;
        Ok(SendRef {
            cell: self.elements[pipe.idx as usize].get_mut(),
            pipe,
        })
    }

    fn try_dequeue(&self) -> Option<T> {
        let res = self.core.try_dequeue()?;
        let idx = res.idx as usize;
        let val = self.elements[idx].with_mut(|ptr| unsafe { (*ptr).assume_init_read() });
        self.core.queue[idx].fetch_add(SEQ_ONE, Ordering::Release);
        Some(val)
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
pub enum DequeueError {
    Closed,
}
