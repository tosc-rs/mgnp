use super::error::*;
use crate::loom::{
    cell::UnsafeCell,
    hint,
    sync::atomic::{self, AtomicU16, AtomicU32, AtomicUsize, Ordering::*},
};
use core::{
    cmp, fmt,
    marker::PhantomData,
    mem::{ManuallyDrop, MaybeUninit},
    slice,
};
use maitake_sync::{WaitCell, WaitQueue};
use mnemos_bitslab::index::IndexAllocWord;
use serde::{de::DeserializeOwned, Serialize};

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

pub(super) struct Core {
    // TODO(eliza): should maybe cache-pad `dequeue_pos`` and `enqueue_pos` on
    // architectures with big cache lines...?
    //
    // or, we could lay the struct out so that other fields provide "free" padding...
    dequeue_pos: AtomicU32,
    cons_wait: WaitCell,
    enqueue_pos: AtomicU32,
    pub(super) prod_wait: WaitQueue,
    indices: IndexAllocWord,
    queue: [AtomicU16; MAX_CAPACITY],
    /// Tracks the state of the the channel's senders/receivers, including
    /// whether a receiver has been claimed, whether the receiver has closed the
    /// channel (e.g. is dropped), and the number of active senders.
    state: AtomicUsize,

    /// The queue's capacity limit.
    ///
    /// This is the length of the actual queue elements array (which is not part
    /// of this struct).
    pub(super) capacity: u8,
}

pub(super) struct Reservation<'core> {
    core: &'core Core,
    pub(super) idx: u8,
}

/// Erases both a pipe and its element type.
pub(super) struct ErasedPipe {
    ptr: *const (),
    vtable: &'static CoreVtable,
}

pub(super) struct TypedPipe<T: 'static> {
    pipe: ErasedPipe,
    _t: PhantomData<fn(T)>,
}

/// A type-erased slice.
#[derive(Copy, Clone)]
pub(super) struct ErasedSlice {
    ptr: *const (),
    len: usize,
    #[cfg(debug_assertions)]
    typ: core::any::TypeId,
}

pub(super) struct CoreVtable {
    pub(super) get_core: unsafe fn(*const ()) -> *const Core,
    pub(super) get_elems: unsafe fn(*const ()) -> ErasedSlice,
    pub(super) clone: unsafe fn(*const ()),
    pub(super) drop: unsafe fn(*const ()),
}

pub(super) struct SerVtable {
    #[cfg(any(test, feature = "alloc"))]
    pub(super) to_vec: SerVecFn,
    #[cfg(any(test, feature = "alloc"))]
    pub(super) to_vec_framed: SerVecFn,
    pub(super) to_slice: SerFn,
    pub(super) to_slice_framed: SerFn,
}

pub(super) struct DeserVtable {
    pub(super) from_bytes: DeserFn,
    pub(super) from_bytes_framed: DeserFn,
}

type SerFn = fn(ErasedSlice, u8, &mut [u8]) -> postcard::Result<&mut [u8]>;

#[cfg(any(test, feature = "alloc"))]
type SerVecFn = fn(ErasedSlice, u8) -> postcard::Result<Vec<u8>>;

pub(super) type DeserFn = fn(ErasedSlice, u8, &[u8]) -> postcard::Result<()>;

/// Values for the `core.state` bitfield.
mod state {
    /// If set, the channel's receiver has been claimed, indicating that no
    /// additional receivers can be claimed.
    pub(super) const RX_CLAIMED: usize = 1 << 1;

    /// If set, the channel's receiver has been dropped. This implies that the
    /// channel is closed by the receive side.
    pub(super) const RX_CLOSED: usize = 1 << 2;

    /// If set, a sender has been created.
    pub(super) const TX_CLOSED: usize = 1 << 3;

    /// Sender reference count; value of one sender.
    pub(super) const TX_ONE: usize = 1 << 4;

    /// Mask for extracting sender reference count.
    pub(super) const TX_MASK: usize = !(RX_CLAIMED | RX_CLOSED | TX_CLOSED);
}

pub(super) const MAX_CAPACITY: usize = IndexAllocWord::MAX_CAPACITY as usize;
const CLOSED_BIT: u32 = 0b1;
const POS_ONE: u32 = 1 << 1;
const SHIFT: u32 = MAX_CAPACITY.trailing_zeros() as u32;
const SEQ_ONE: u16 = 1 << SHIFT;
const MASK: u32 = SEQ_ONE as u32 - 1;

// === impl Core ===

impl Core {
    #[cfg(not(loom))]
    pub(super) const fn new(capacity: u8) -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const QUEUE_INIT: AtomicU16 = AtomicU16::new(0);

        debug_assert!(capacity <= MAX_CAPACITY as u8);
        let mut queue = [QUEUE_INIT; MAX_CAPACITY];
        let mut i = 0;

        while i != MAX_CAPACITY {
            queue[i] = AtomicU16::new((i as u16) << SHIFT);
            i += 1;
        }

        Core {
            dequeue_pos: AtomicU32::new(0),
            enqueue_pos: AtomicU32::new(0),
            cons_wait: WaitCell::new(),
            prod_wait: WaitQueue::new(),
            indices: IndexAllocWord::new(),
            queue,
            state: AtomicUsize::new(0),
            capacity,
        }
    }

    // this can't be a const fn when running loom tests, since constructing an
    // atomic is not const.
    #[cfg(loom)]
    pub(super) fn new(capacity: u8) -> Self {
        debug_assert!(capacity <= MAX_CAPACITY as u8);
        let queue = {
            // this is, unfortunately, the nicest way to initialize an array of
            // loom atomics, since they don't have a `const fn` constructor. :(
            // oh well, this is test-only code...
            let vec = (0..MAX_CAPACITY)
                .map(|i| AtomicU16::new((i as u16) << SHIFT))
                .collect::<alloc::vec::Vec<_>>();
            <[_; MAX_CAPACITY]>::try_from(vec).expect("vec should be the correct length")
        };

        Core {
            dequeue_pos: AtomicU32::new(0),
            enqueue_pos: AtomicU32::new(0),
            cons_wait: WaitCell::new(),
            prod_wait: WaitQueue::new(),
            indices: IndexAllocWord::new(),
            queue,
            state: AtomicUsize::new(0),
            capacity,
        }
    }

    pub(super) fn try_reserve(&self) -> Result<Reservation<'_>, TrySendError> {
        test_span!("Core::try_reserve");
        if test_dbg!(self.state.load(Acquire)) & state::RX_CLOSED == state::RX_CLOSED {
            return Err(TrySendError::Closed);
        }
        test_dbg!(self.indices.allocate())
            .ok_or(TrySendError::Full)
            .map(|idx| Reservation { core: self, idx })
    }

    pub(super) async fn reserve(&self) -> Result<Reservation, SendError> {
        loop {
            match self.try_reserve() {
                Ok(res) => return Ok(res),
                Err(TrySendError::Closed) => return Err(SendError::Closed),
                Err(TrySendError::Full) => {
                    self.prod_wait.wait().await.map_err(|_| SendError::Closed)?
                }
            }
        }
    }

    pub(super) async fn dequeue(&self) -> Option<Reservation<'_>> {
        loop {
            match self.try_dequeue() {
                Ok(res) => return Some(res),
                Err(TryRecvError::Closed) => return None,
                Err(TryRecvError::Empty) => {
                    // we never close the rx waitcell, because the
                    // rx is responsible for determining if the channel is
                    // closed by the tx: there may be messages in the channel to
                    // consume before the rx considers it properly closed.
                    let _ = test_dbg!(self.cons_wait.wait().await);
                    hint::spin_loop();
                }
            }
        }
    }

    pub(super) fn try_dequeue(&self) -> Result<Reservation<'_>, TryRecvError> {
        test_span!("Core::try_dequeue");
        let mut head = test_dbg!(self.dequeue_pos.load(Relaxed));
        loop {
            let pos = head >> 1;
            let slot = &self.queue[(pos & MASK) as usize];
            let val = test_dbg!(slot.load(Acquire));
            let seq = test_dbg!(val >> SHIFT);
            let dif = test_dbg!(seq as i8).wrapping_sub(pos.wrapping_add(1) as i8);

            match test_dbg!(dif).cmp(&0) {
                cmp::Ordering::Less if head & CLOSED_BIT != 0 => return Err(TryRecvError::Closed),
                cmp::Ordering::Less => return Err(TryRecvError::Empty),
                cmp::Ordering::Equal => match test_dbg!(self.dequeue_pos.compare_exchange_weak(
                    head,
                    head.wrapping_add(POS_ONE),
                    Relaxed,
                    Relaxed,
                )) {
                    Ok(_) => {
                        slot.store(val.wrapping_add(SEQ_ONE), Release);
                        return Ok(Reservation {
                            core: self,
                            idx: (val & MASK as u16) as u8,
                        });
                    }
                    Err(actual) => head = actual,
                },
                cmp::Ordering::Greater => head = test_dbg!(self.dequeue_pos.load(Relaxed)),
            }
        }
    }

    fn commit_send(&self, idx: u8) {
        test_span!("Core::commit_send", idx);
        debug_assert!(idx as u32 <= MASK);
        let mut tail = test_dbg!(self.enqueue_pos.load(Relaxed));
        loop {
            let pos = tail >> 1;
            let slot = &self.queue[test_dbg!(pos & MASK) as usize];
            let seq = test_dbg!(slot.load(Acquire)) >> SHIFT;
            let dif = test_dbg!(seq as i8).wrapping_sub(pos as i8);

            match test_dbg!(dif).cmp(&0) {
                cmp::Ordering::Less => unreachable!(),
                cmp::Ordering::Equal => match test_dbg!(self.enqueue_pos.compare_exchange_weak(
                    tail,
                    tail.wrapping_add(POS_ONE),
                    Relaxed,
                    Relaxed,
                )) {
                    Ok(_) => {
                        let new = test_dbg!(test_dbg!((pos as u16) << SHIFT).wrapping_add(SEQ_ONE));
                        slot.store(test_dbg!(idx as u16 | new), Release);
                        test_dbg!(self.cons_wait.wake());
                        return;
                    }
                    Err(actual) => tail = actual,
                },
                cmp::Ordering::Greater => tail = test_dbg!(self.enqueue_pos.load(Relaxed)),
            }
        }
    }

    unsafe fn uncommit(&self, idx: u8) {
        test_println!(idx, "Core::uncommit");
        self.indices.free(idx);
        self.prod_wait.wake();
    }

    pub(super) fn try_claim_rx(&self) -> Option<()> {
        // set `RX_CLAIMED`.
        let state = test_dbg!(self.state.fetch_or(state::RX_CLAIMED, AcqRel));
        // if the `RX_CLAIMED` bit was not set, we successfully claimed the
        // receiver.
        let claimed = test_dbg!(state & state::RX_CLAIMED) == 0;
        claimed.then_some(())
    }

    /// Close the channel from the receiver.
    pub(super) fn close_rx(&self) {
        // set the state to indicate that the receiver was dropped.
        test_dbg!(self.state.fetch_or(state::RX_CLOSED, Release));
        // notify any waiting senders that the channel is closed.
        self.prod_wait.close();
    }

    #[inline]
    pub(super) fn add_tx(&self) {
        // increment the sender reference count.
        test_dbg!(self.state.fetch_add(state::TX_ONE, Relaxed));
    }

    /// Drop a sender
    #[inline]
    pub(super) fn drop_tx(&self) {
        let val = test_dbg!(self.state.fetch_sub(state::TX_ONE, Relaxed));
        if test_dbg!(val & state::TX_MASK == state::TX_ONE) {
            test_dbg!(self.dequeue_pos.fetch_or(CLOSED_BIT, Release));
            self.cons_wait.close();
        }
    }

    #[must_use]
    pub(super) fn is_empty(&self) -> bool {
        // This *could* be `self.len() == 0` but it's more efficient to avoid
        // the loop waiting to get a consistent snapshot.
        let enqueue_pos = self.enqueue_pos.load(SeqCst);
        let dequeue_pos = self.dequeue_pos.load(SeqCst);
        // Note that, unlike in the `len()` function, we don't need to reload
        // the dequeue index, because if the enqueue index changed under us,
        // that means the queue was not empty when we snapshotted, and it's fine
        // to say so.
        enqueue_pos == dequeue_pos
    }

    #[must_use]
    pub(super) fn is_full(&self) -> bool {
        // This could be `self.len() == self.capacity()` but this is more
        // efficient.
        let enqueue_pos = self.enqueue_pos.load(SeqCst);
        let dequeue_pos = self.dequeue_pos.load(SeqCst);

        // If the dequeue index has lagged behind the enqueue index by an entire
        // "lap" around the ring buffer, then the queue is full.
        dequeue_pos.wrapping_add(SEQ_ONE as u32) == enqueue_pos
    }

    #[must_use]
    pub(super) fn len(&self) -> usize {
        loop {
            // Load both the enqueue and dequeue indices.
            let enqueue_pos = self.enqueue_pos.load(SeqCst);
            let dequeue_pos = self.dequeue_pos.load(SeqCst);

            // If the enqueue index hasn't changed while we were loading the
            // dequeue index, then we have a consistent snapshot of both
            // indices.
            if self.enqueue_pos.load(SeqCst) == enqueue_pos {
                let head = dequeue_pos & MASK;
                let tail = enqueue_pos & MASK;

                return match head.cmp(&tail) {
                    cmp::Ordering::Less => (tail - head) as usize,
                    cmp::Ordering::Equal => 0,
                    cmp::Ordering::Greater => self.capacity as usize - (head + tail) as usize,
                };
            }
        }
    }
}

// === impl Reservation ===

impl Reservation<'_> {
    pub(super) fn commit_send(self) {
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

impl fmt::Debug for Reservation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { core, idx } = self;
        f.debug_struct("Reservation")
            .field("core", &format_args!("{core:#p}"))
            .field("idx", idx)
            .finish()
    }
}

impl ErasedSlice {
    pub(super) fn erase<T: 'static>(slice: impl AsRef<[T]>) -> Self {
        let slice = slice.as_ref();
        let len = slice.len();
        Self {
            ptr: slice.as_ptr().cast(),
            len,

            #[cfg(debug_assertions)]
            typ: core::any::TypeId::of::<T>(),
        }
    }

    unsafe fn unerase<'a, T: 'static>(self) -> &'a [T] {
        #[cfg(debug_assertions)]
        debug_assert_eq!(
            self.typ,
            core::any::TypeId::of::<T>(),
            "/!\\ EXTREMELY SERIOUS WARNING: you would have just done a type confusion, this is Real Bad"
        );
        slice::from_raw_parts(self.ptr.cast(), self.len)
    }
}

impl ErasedPipe {
    pub(super) unsafe fn new(ptr: *const (), vtable: &'static CoreVtable) -> Self {
        Self { ptr, vtable }
    }

    /// # Safety
    ///
    /// This `ErasedPipe` must have been type-erased from a tricky-pipe with
    /// elements of type `T`!
    pub(super) unsafe fn typed<T>(self) -> TypedPipe<T> {
        TypedPipe {
            pipe: self,
            _t: PhantomData,
        }
    }

    pub(super) fn core(&self) -> &Core {
        unsafe { &*(self.vtable.get_core)(self.ptr) }
    }

    pub(super) fn elems(&self) -> ErasedSlice {
        unsafe { (self.vtable.get_elems)(self.ptr) }
    }
}

impl Clone for ErasedPipe {
    fn clone(&self) -> Self {
        unsafe { (self.vtable.clone)(self.ptr) }
        Self {
            ptr: self.ptr,
            vtable: self.vtable,
        }
    }
}

impl Drop for ErasedPipe {
    fn drop(&mut self) {
        unsafe { (self.vtable.drop)(self.ptr) }
    }
}

// === impl TypedPipe ===

impl<T: 'static> TypedPipe<T> {
    pub(super) fn core(&self) -> &Core {
        self.pipe.core()
    }

    pub(super) fn elems(&self) -> &[UnsafeCell<MaybeUninit<T>>] {
        unsafe { self.pipe.elems().unerase::<UnsafeCell<MaybeUninit<T>>>() }
    }
}

impl<T: 'static> Clone for TypedPipe<T> {
    fn clone(&self) -> Self {
        Self {
            pipe: self.pipe.clone(),
            _t: PhantomData,
        }
    }
}

unsafe impl<T: Send> Send for TypedPipe<T> {}
unsafe impl<T: Send> Sync for TypedPipe<T> {}

impl SerVtable {
    pub(super) fn to_slice<T: Serialize + 'static>(
        elems: ErasedSlice,
        idx: u8,
        buf: &mut [u8],
    ) -> postcard::Result<&mut [u8]> {
        unsafe {
            let elems = elems.unerase::<UnsafeCell<MaybeUninit<T>>>();
            elems[idx as usize].with(|ptr| {
                let elem = (*ptr).assume_init_ref();
                postcard::to_slice(elem, buf)
            })
        }
    }

    pub(super) fn to_slice_framed<T: Serialize + 'static>(
        elems: ErasedSlice,
        idx: u8,
        buf: &mut [u8],
    ) -> postcard::Result<&mut [u8]> {
        unsafe {
            // TODO(eliza): since this is unsafe anyway, we *could* just do
            // pointer math and elide the bounds check... &shrug;
            let elems = elems.unerase::<UnsafeCell<MaybeUninit<T>>>();
            elems[idx as usize].with(|ptr| {
                let elem = (*ptr).assume_init_ref();
                postcard::to_slice_cobs(elem, buf)
            })
        }
    }

    #[cfg(any(test, feature = "alloc"))]
    pub(super) fn to_vec<T: Serialize + 'static>(
        elems: ErasedSlice,
        idx: u8,
    ) -> postcard::Result<Vec<u8>> {
        unsafe {
            let elems = elems.unerase::<UnsafeCell<MaybeUninit<T>>>();
            elems[idx as usize].with(|ptr| {
                let elem = (*ptr).assume_init_ref();
                postcard::to_allocvec(elem)
            })
        }
    }

    #[cfg(any(test, feature = "alloc"))]
    pub(super) fn to_vec_framed<T: Serialize + 'static>(
        elems: ErasedSlice,
        idx: u8,
    ) -> postcard::Result<Vec<u8>> {
        unsafe {
            let elems = elems.unerase::<UnsafeCell<MaybeUninit<T>>>();
            elems[idx as usize].with(|ptr| {
                let elem = (*ptr).assume_init_ref();
                postcard::to_allocvec_cobs(elem)
            })
        }
    }
}

impl DeserVtable {
    pub(super) const fn new<T: DeserializeOwned + 'static>() -> Self {
        Self {
            from_bytes: Self::from_bytes::<T>,
            from_bytes_framed: Self::from_bytes_framed::<T>,
        }
    }

    fn from_bytes<T: DeserializeOwned + 'static>(
        elems: ErasedSlice,
        idx: u8,
        buf: &[u8],
    ) -> postcard::Result<()> {
        let val = postcard::from_bytes(buf)?;
        unsafe {
            let elems = elems.unerase::<UnsafeCell<MaybeUninit<T>>>();
            elems[idx as usize].with_mut(|ptr| (*ptr).write(val));
        }
        Ok(())
    }

    fn from_bytes_framed<T: DeserializeOwned + 'static>(
        elems: ErasedSlice,
        idx: u8,
        buf: &[u8],
    ) -> postcard::Result<()> {
        let val = postcard::from_bytes(buf)?;
        unsafe {
            let elems = elems.unerase::<UnsafeCell<MaybeUninit<T>>>();
            elems[idx as usize].with_mut(|ptr| (*ptr).write(val));
        }
        Ok(())
    }
}
