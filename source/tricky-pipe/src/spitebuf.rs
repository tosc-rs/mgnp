//! SpiteBuf - an Async (or not, I'm not a cop) MpscQueue
//!
//! Based on some stuff
//!
//! # References
//!
//! This is an implementation of Dmitry Vyukov's ["Bounded MPMC queue"][0] minus the cache padding.
//!
//! Queue implementation from heapless::mpmc::MpMcQueue
//!
//! [0]: http://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue

#![allow(clippy::missing_safety_doc)]

use core::marker::PhantomData;
use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
};
use maitake_sync::{WaitCell, WaitQueue};
use portable_atomic::{AtomicU8, AtomicUsize, Ordering};


pub unsafe trait Storage<T> {
    fn buf(&self) -> (*const UnsafeCell<Cell<T>>, usize);
}

pub struct MpScQueue<T, STO: Storage<T>> {
    storage: STO,
    dequeue_pos: AtomicUsize,
    enqueue_pos: AtomicUsize,
    cons_wait: WaitCell,
    prod_wait: WaitQueue,
    state: AtomicU8,
    pd: PhantomData<T>,
}

const STATE_UNINIT: u8 = 0;
const STATE_INITING: u8 = 1;
const STATE_SPLIT: u8 = 3;
// TODO: refcount access? some kind of ctr? Some way of handling
// re-opening cases? Use fancy mycelium bitfield?
const STATE_CLOSED: u8 = 4;


/// Represents a closed error
#[derive(Debug, Eq, PartialEq)]
pub enum EnqueueError<T> {
    Full(T),
    Closed(T),
    InternalError(T),
}

#[derive(Debug, Eq, PartialEq)]
pub enum DequeueError {
    Closed,
}

pub struct Sender<T, STO>
where
    T: 'static,
    STO: Storage<T> + 'static,
{
    q: &'static MpScQueue<T, STO>,
}

impl<T, STO> Sender<T, STO>
where
    T: 'static,
    STO: Storage<T> + 'static,
{
    /// Adds an `item` to the end of the queue
    ///
    /// Returns back the `item` if the queue is full
    #[inline(always)]
    pub fn enqueue_sync(&self, item: T) -> Result<(), EnqueueError<T>> {
        self.q.enqueue_sync(item)
    }

    #[inline(always)]
    pub async fn enqueue_async(&self, item: T) -> Result<(), EnqueueError<T>> {
        self.q.enqueue_async(item).await
    }
}

pub struct Receiver<T, STO>
where
    T: 'static,
    STO: Storage<T> + 'static,
{
    q: &'static MpScQueue<T, STO>,
}

impl<T, STO> Receiver<T, STO>
where
    T: 'static,
    STO: Storage<T> + 'static,
{
    /// Returns the item in the front of the queue, or `None` if the queue is empty
    #[inline(always)]
    pub fn dequeue_sync(&self) -> Option<T> {
        self.q.dequeue_sync()
    }

    #[inline(always)]
    pub async fn dequeue_async(&self) -> Result<T, DequeueError> {
        self.q.dequeue_async().await
    }
}

impl<T, STO: Storage<T>> MpScQueue<T, STO> {
    pub const fn new_uninit(storage: STO) -> Self {
        Self {
            storage,
            dequeue_pos: AtomicUsize::new(0),
            enqueue_pos: AtomicUsize::new(0),
            cons_wait: WaitCell::new(),
            prod_wait: WaitQueue::new(),
            state: AtomicU8::new(STATE_UNINIT),
            pd: PhantomData,
        }
    }

    /// Creates an empty queue
    ///
    /// The capacity of `storage` must be >= 2 and a power of two, or this code will panic.
    pub fn init_split(&'static self) -> Result<(Sender<T, STO>, Receiver<T, STO>), ()> {
        let (ptr, len) = self.storage.buf();
        assert_eq!(
            len,
            len.next_power_of_two(),
            "Capacity must be a power of two!"
        );
        assert!(len > 1, "Capacity must be larger than 1!");

        match self.state.compare_exchange(STATE_UNINIT, STATE_INITING, Ordering::AcqRel, Ordering::AcqRel) {
            Ok(_) => {},
            Err(_) => return Err(()),
        }
        // We now have exclusive access to the field.

        let sli = unsafe { core::slice::from_raw_parts(ptr, len) };
        sli.iter().enumerate().for_each(|(i, slot)| unsafe {
            slot.get().write(Cell {
                data: MaybeUninit::uninit(),
                sequence: AtomicUsize::new(i),
            });
        });

        self.state.store(STATE_SPLIT, Ordering::Release);

        Ok((Sender { q: self }, Receiver { q: self }))
    }

    /// Returns the item in the front of the queue, or `None` if the queue is empty
    fn dequeue_sync(&self) -> Option<T> {
        // Note: DON'T check the closed flag on dequeue. We want to be able
        // to drain any potential messages after closing.
        let (ptr, len) = self.storage.buf();
        let res = unsafe { dequeue((*ptr).get(), &self.dequeue_pos, len - 1) };
        if res.is_some() {
            self.prod_wait.wake_all();
        }
        res
    }

    async fn dequeue_async(&self) -> Result<T, DequeueError> {
        loop {
            // note: this future always completes immediately, it's awaited just
            // to grab the wait future with a preregistered waker.
            let wait = self.cons_wait.subscribe().await;
            match self.dequeue_sync() {
                Some(t) => return Ok(t),

                // Note: if we have been closed, this wait will fail.
                None => match wait.await {
                    Ok(()) => {}
                    Err(_) => return Err(DequeueError::Closed),
                },
            }
        }
    }

    /// Adds an `item` to the end of the queue
    ///
    /// Returns back the `item` if the queue is full
    pub fn enqueue_sync(&self, item: T) -> Result<(), EnqueueError<T>> {
        match self.state.load(Ordering::Acquire) {
            STATE_SPLIT => {},
            STATE_CLOSED => return Err(EnqueueError::Closed(item)),
            _ => return Err(EnqueueError::InternalError(item)),
        }
        let (ptr, len) = self.storage.buf();
        let res = unsafe { enqueue((*ptr).get(), &self.enqueue_pos, len - 1, item) };
        if res.is_ok() {
            self.cons_wait.wake();
        }
        res.map_err(EnqueueError::Full)
    }

    pub async fn enqueue_async(&self, mut item: T) -> Result<(), EnqueueError<T>> {
        loop {
            match self.enqueue_sync(item) {
                // We succeeded or the queue is closed, propagate those errors
                ok @ Ok(_) => return ok,
                err @ Err(EnqueueError::Closed(_)) | err @ Err(EnqueueError::InternalError(_)) => return err,

                // It's full, let's wait until it isn't or the channel has closed
                Err(EnqueueError::Full(eitem)) => {
                    match self.prod_wait.wait().await {
                        Ok(()) => {}
                        Err(_) => return Err(EnqueueError::Closed(eitem)),
                    }
                    item = eitem;
                }
            }
        }
    }

    // Mark the channel as permanently closed. Any already sent data
    // can be retrieved, but no further data will be allowed to be pushed.
    pub fn close(&self) {
        match self.state.compare_exchange(STATE_SPLIT, STATE_CLOSED, Ordering::AcqRel, Ordering::AcqRel) {
            Ok(_) => {
                self.cons_wait.close();
                self.prod_wait.close();
            },
            Err(_) => todo!(),
        }
    }
}

unsafe impl<T, STO: Storage<T>> Sync for MpScQueue<T, STO> where T: Send {}

impl<T, STO: Storage<T>> Drop for MpScQueue<T, STO> {
    fn drop(&mut self) {
        while self.dequeue_sync().is_some() {}
        self.cons_wait.close();
        self.prod_wait.close();
    }
}

pub struct Cell<T> {
    data: MaybeUninit<T>,
    sequence: AtomicUsize,
}

pub const fn single_cell<T>() -> Cell<T> {
    Cell {
        data: MaybeUninit::uninit(),
        sequence: AtomicUsize::new(0),
    }
}

pub fn cell_array<const N: usize, T: Sized>() -> [Cell<T>; N] {
    [Cell::<T>::SINGLE_CELL; N]
}

impl<T> Cell<T> {
    const SINGLE_CELL: Self = Self::new(0);

    const fn new(seq: usize) -> Self {
        Self {
            data: MaybeUninit::uninit(),
            sequence: AtomicUsize::new(seq),
        }
    }
}

unsafe fn dequeue<T>(buffer: *mut Cell<T>, dequeue_pos: &AtomicUsize, mask: usize) -> Option<T> {
    let mut pos = dequeue_pos.load(Ordering::Relaxed);

    let mut cell;
    loop {
        cell = buffer.add(pos & mask);
        let seq = (*cell).sequence.load(Ordering::Acquire);
        let dif = (seq as i8).wrapping_sub((pos.wrapping_add(1)) as i8);

        match dif {
            0 => {
                if dequeue_pos
                    .compare_exchange_weak(
                        pos,
                        pos.wrapping_add(1),
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    break;
                }
            }
            dif if dif < 0 => return None,
            _ => pos = dequeue_pos.load(Ordering::Relaxed),
        }
    }

    let data = (*cell).data.as_ptr().read();
    (*cell)
        .sequence
        .store(pos.wrapping_add(mask).wrapping_add(1), Ordering::Release);
    Some(data)
}

unsafe fn enqueue<T>(
    buffer: *mut Cell<T>,
    enqueue_pos: &AtomicUsize,
    mask: usize,
    item: T,
) -> Result<(), T> {
    let mut pos = enqueue_pos.load(Ordering::Relaxed);

    let mut cell;
    loop {
        cell = buffer.add(pos & mask);
        let seq = (*cell).sequence.load(Ordering::Acquire);
        let dif = (seq as i8).wrapping_sub(pos as i8);

        match dif {
            0 => {
                if enqueue_pos
                    .compare_exchange_weak(
                        pos,
                        pos.wrapping_add(1),
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    break;
                }
            }
            dif if dif < 0 => return Err(item),
            _ => pos = enqueue_pos.load(Ordering::Relaxed),
        }
    }

    (*cell).data.as_mut_ptr().write(item);
    (*cell)
        .sequence
        .store(pos.wrapping_add(1), Ordering::Release);
    Ok(())
}
