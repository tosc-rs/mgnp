use super::error::*;
use crate::loom::{
    cell::UnsafeCell,
    hint,
    sync::atomic::{AtomicU16, AtomicUsize, Ordering::*},
};
use core::{
    cmp, fmt,
    marker::PhantomData,
    mem::{ManuallyDrop, MaybeUninit},
    slice,
    task::{self, Context, Poll},
};
use maitake_sync::{WaitCell, WaitQueue};
use mnemos_bitslab::index::IndexAllocWord;
use serde::{de::DeserializeOwned, Serialize};

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

pub(super) struct Core<E> {
    // === receiver-only state ====
    /// The head of the queue (i.e. the position at which elements are popped by
    /// the receiver).
    ///
    /// This value consists of a one-bit flag indicating that the queue has been
    /// closed by the sender, an index into the queue array, and the sequence
    /// number (the current lap around the queue array). The closed flag is
    /// represented by the [`CLOSED`] constant. The index is represented by the
    /// next [`SEQ_SHIFT`] bits (5 bits on 32-bit machines or 6 bits on 64-bit
    /// machines). Finally, the remaining 9 or 10 bits are the sequence number.
    ///
    /// Since we always have a maximum capacity of 32 or 64 elements, a 16-bit
    /// number is always sufficient to hold all indices in the array, the
    /// [`CLOSED`] bit, and a sufficiently large sequence number to prevent the
    /// ABA problem.
    // TODO(eliza): should maybe cache-pad `dequeue_pos` and `enqueue_pos` on
    // architectures with big cache lines...?
    //
    // or, we could lay the struct out so that other fields provide "free"
    // padding...
    dequeue_pos: AtomicU16,
    /// WaitCell for the receiver when it's waiting for a message to be
    /// enqueued.
    cons_wait: WaitCell,

    // === sender-only state ===
    /// The tail of the queue (i.e. the position at which elements are pushed by
    /// the sender).
    ///
    /// This value consists of a one-bit flag indicating that the queue has been
    /// closed by the receiver, an index into the queue array, and the sequence
    /// number (the current lap around the queue array). The closed flag is
    /// represented by the [`CLOSED`] constant. The index is represented by the
    /// next [`SEQ_SHIFT`] bits (5 bits on 32-bit machines or 6 bits on 64-bit
    /// machines). Finally, the remaining 9 or 10 bits are the sequence number.
    ///
    /// Since we always have a maximum capacity of 32 or 64 elements, a 16-bit
    /// number is always sufficient to hold all indices in the array, the
    /// [`CLOSED`] bit, and a sufficiently large sequence number to prevent the
    /// ABA problem.
    enqueue_pos: AtomicU16,
    /// WaitQueue for senders waiting for queue capacity.
    pub(super) prod_wait: WaitQueue,

    // === shared state ===
    /// Index allocator used by the sender.
    indices: IndexAllocWord,
    /// The actual array used to represent the queue of sent indices.
    queue: [AtomicU16; MAX_CAPACITY],
    /// Tracks the state of the the channel's senders/receivers, including
    /// whether a receiver has been claimed and the number of active senders.
    state: AtomicUsize,
    /// The queue's capacity limit.
    ///
    /// This is the length of the actual queue elements array (which is not part
    /// of this struct).
    pub(super) capacity: u8,
    /// If the channel closed with an error, this is the error.
    error: UnsafeCell<MaybeUninit<E>>,
}

pub(super) struct Reservation<'core, E> {
    core: &'core Core<E>,
    pub(super) idx: u8,
}

/// Erases both a pipe and its element type.
pub(super) struct ErasedPipe<E: 'static> {
    ptr: *const (),
    vtable: &'static CoreVtable<E>,
}

pub(super) struct TypedPipe<T: 'static, E: 'static> {
    pipe: ErasedPipe<E>,
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

pub(super) struct CoreVtable<E> {
    pub(super) get_core: unsafe fn(*const ()) -> *const Core<E>,
    pub(super) get_elems: unsafe fn(*const ()) -> ErasedSlice,
    pub(super) clone: unsafe fn(*const ()),
    pub(super) drop: unsafe fn(*const ()),
    pub(super) type_name: fn() -> &'static str,
}

pub(super) struct SerVtable {
    #[cfg(any(test, feature = "alloc"))]
    pub(super) to_vec: SerVecFn,
    #[cfg(any(test, feature = "alloc"))]
    pub(super) to_vec_framed: SerVecFn,
    pub(super) to_slice: SerFn,
    pub(super) to_slice_framed: SerFn,
    pub(super) drop_elem: unsafe fn(ErasedSlice, u8),
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
    pub(super) const RX_CLAIMED: usize = 1 << 0;

    /// Sender reference count; value of one sender.
    pub(super) const TX_ONE: usize = 1 << TX_SHIFT;

    /// Offset of TX count, in bits
    pub(super) const TX_SHIFT: usize = 1;
}

pub(super) const MAX_CAPACITY: usize = IndexAllocWord::MAX_CAPACITY as usize;

/// Bit in `enqueue_pos` and `dequeue_pos` indicating that the channel has been
/// closed by the other side (the senders, in `dequeue_pos`, and the receiver,
/// in `enqueue_pos`).
///
/// This is the first bit of the pos word, so that it is not clobbered if
/// incrementing the actual position in the queue wraps around (which is fine).
const CLOSED: u16 = 1 << 0;
const HAS_ERROR: u16 = 1 << 1;
const CLOSED_ERROR: u16 = CLOSED | HAS_ERROR;
const POS_SHIFT: u16 = CLOSED_ERROR.trailing_ones() as u16;
/// The value by which `enqueue_pos` and `dequeue_pos` are incremented. This is
/// shifted left by two to account for the lowest bits being used for `CLOSED`
/// and `HAS_ERROR`
const POS_ONE: u16 = 1 << POS_SHIFT;
const MASK: u16 = MAX_CAPACITY as u16 - 1;
const SEQ_SHIFT: u16 = MASK.trailing_ones() as u16;
const SEQ_ONE: u16 = 1 << SEQ_SHIFT;

// === impl Core ===

impl<E> Core<E> {
    #[cfg(not(loom))]
    pub(super) const fn new(capacity: u8) -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const QUEUE_INIT: AtomicU16 = AtomicU16::new(0);

        debug_assert!(capacity <= MAX_CAPACITY as u8);
        let mut queue = [QUEUE_INIT; MAX_CAPACITY];
        let mut i = 0;

        while i != MAX_CAPACITY {
            queue[i] = AtomicU16::new((i as u16) << SEQ_SHIFT);
            i += 1;
        }

        Core {
            dequeue_pos: AtomicU16::new(0),
            enqueue_pos: AtomicU16::new(0),
            cons_wait: WaitCell::new(),
            prod_wait: WaitQueue::new(),
            indices: IndexAllocWord::with_capacity(capacity),
            queue,
            // The state starts out with the value of `TX_ONE`, since the
            // `TrickyPipe` itself can create new senders freely. The channel
            // only closes when the `TrickyPipe` *and* all senders have been
            // dropped.
            state: AtomicUsize::new(state::TX_ONE),
            capacity,
            error: UnsafeCell::new(MaybeUninit::uninit()),
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
                .map(|i| AtomicU16::new((i as u16) << SEQ_SHIFT))
                .collect::<alloc::vec::Vec<_>>();
            <[_; MAX_CAPACITY]>::try_from(vec).expect("vec should be the correct length")
        };

        Core {
            dequeue_pos: AtomicU16::new(0),
            enqueue_pos: AtomicU16::new(0),
            cons_wait: WaitCell::new(),
            prod_wait: WaitQueue::new(),
            indices: IndexAllocWord::with_capacity(capacity),
            queue,
            state: AtomicUsize::new(state::TX_ONE),
            capacity,

            error: UnsafeCell::new(MaybeUninit::uninit()),
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
        test_println!(claimed, "Core::try_claim_rx");
        claimed.then_some(())
    }

    /// Close the channel from the receiver.
    pub(super) fn close_rx(&self) {
        // set the state to indicate that the receiver was dropped.
        test_dbg!(self.enqueue_pos.fetch_or(CLOSED, Release));
        // notify any waiting senders that the channel is closed.
        self.prod_wait.close();
        test_println!("Core::close_rx: -> closed");
    }

    pub(super) fn close_rx_error(&self, error: E) {
        // store the error in the channel.
        self.error.with_mut(|ptr| unsafe {
            // Safety: this is okay, because there is only one receiver, and the
            // senders will not attempt to access the error until the receiver
            // has set the `CLOSED_ERROR` bits.
            //
            // The receiver will not close the channel more than once.
            (*ptr).write(error);
        });
        // set the state to indicate that the receiver closed the channel.
        test_dbg!(self.enqueue_pos.fetch_or(CLOSED_ERROR, Release));
        // notify any waiting senders that the channel is closed.
        self.prod_wait.close();
        test_println!("Core::close_rx_error: -> closed");
    }

    #[inline]
    pub(super) fn add_tx(&self) {
        // Using a relaxed ordering is alright here, as knowledge of the
        // original reference (the `Sender` that was cloned, or the `TrickyPipe`
        // which is constructing the new `Sender`) prevents other threads from
        // erroneously closing the channel.
        //
        // As explained in the [Boost documentation][1], Increasing the
        // reference counter can always be done with memory_order_relaxed: New
        // references to an object can only be formed from an existing
        // reference, and passing an existing reference from one thread to
        // another must already provide any required synchronization.
        //
        // [1]: (www.boost.org/doc/libs/1_55_0/doc/html/atomic/usage_examples.html)
        let _refs = test_dbg!(self.state.fetch_add(state::TX_ONE, Relaxed));
        test_println!(refs = _refs, "Core::add_tx");
    }

    /// Drop a sender
    #[inline]
    pub(super) fn drop_tx(&self) {
        test_span!("Core::drop_tx");
        // Because `fetch_sub` is already atomic, we do not need to synchronize
        // with other threads unless we are going to delete the object. This
        // same logic applies to the below `fetch_sub` to the `weak` count.
        let refs = self.state.fetch_sub(state::TX_ONE, Release) >> state::TX_SHIFT;
        if test_dbg!(refs) == 1 {
            // Ensure that setting the closed bit happens-after all other
            // `Release` adds/subs to `state`. We perform an `Acquire` RMW op on
            // the `state` to ensure that we are now after all other ref count
            // ops. The value from this load is not actually used.
            let _val = test_dbg!(self.state.fetch_or(0, Acquire));
            debug_assert_eq!(_val >> state::TX_SHIFT, 0);
            // Now that we're after all other ref count ops, we can close the
            // channel itself.
            test_dbg!(self.dequeue_pos.fetch_or(CLOSED, Release));
            self.cons_wait.close();

            test_println!("Core::drop_tx -> closed");
        } else {
            test_println!("Core::drop_tx -> tx refs remaining");
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
        dequeue_pos.wrapping_add(SEQ_ONE) == enqueue_pos
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

impl<E: Clone> Core<E> {
    pub(super) fn try_reserve(&self) -> Result<Reservation<'_, E>, TrySendError<E>> {
        test_span!("Core::try_reserve");
        let enqueue_pos = self.enqueue_pos.load(Acquire);
        if test_dbg!(enqueue_pos & CLOSED) == CLOSED {
            return Err(self
                .send_closed_error()
                .map(|error| TrySendError::Error { error, message: () })
                .unwrap_or(TrySendError::Closed(())));
        }

        test_dbg!(self.indices.allocate())
            .ok_or(TrySendError::Full(()))
            .map(|idx| Reservation { core: self, idx })
    }

    pub(super) async fn reserve(&self) -> Result<Reservation<'_, E>, SendError<E>> {
        loop {
            match self.try_reserve() {
                Ok(res) => return Ok(res),
                Err(TrySendError::Closed(())) => return Err(SendError::Closed(())),
                Err(TrySendError::Error { error, .. }) => {
                    return Err(SendError::Error { error, message: () })
                }
                Err(TrySendError::Full(())) => self.prod_wait.wait().await.map_err(|_| {
                    self.send_closed_error()
                        .map(|error| SendError::Error { error, message: () })
                        .unwrap_or(SendError::Closed(()))
                })?,
            }
        }
    }

    pub(super) fn poll_dequeue(
        &self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Reservation<'_, E>, RecvError<E>>> {
        loop {
            match self.try_dequeue() {
                Ok(res) => return Poll::Ready(Ok(res)),
                Err(TryRecvError::Closed) => return Poll::Ready(Err(RecvError::Closed)),
                Err(TryRecvError::Error(error)) => {
                    return Poll::Ready(Err(RecvError::Error(error)))
                }
                Err(TryRecvError::Empty) => {
                    // we never close the rx waitcell, because the
                    // rx is responsible for determining if the channel is
                    // closed by the tx: there may be messages in the channel to
                    // consume before the rx considers it properly closed.
                    let _ = task::ready!(test_dbg!(self.cons_wait.poll_wait(cx)));
                    // if the poll_wait returns ready, then another thread just
                    // enqueued something. sticking a spin loop hint here tells
                    // `loom` that we're waiting for that thread before we can
                    // make progress. in real life, the `PAUSE` instruction or
                    // similar may also help us actually see the other thread's
                    // change...if it takes a single cycle of delay for it to
                    // reflect? idk lol ¯\_(ツ)_/¯
                    hint::spin_loop();
                }
            }
        }
    }

    pub(super) fn try_dequeue(&self) -> Result<Reservation<'_, E>, TryRecvError<E>> {
        test_span!("Core::try_dequeue");
        let mut head = test_dbg!(self.dequeue_pos.load(Acquire));
        loop {
            // Shift to the right to extract the actual position, and
            // discard the `CLOSED` and `HAS_ERROR` bits.
            let pos = head >> POS_SHIFT;
            let slot = &self.queue[(pos & MASK) as usize];
            // Load the slot's current value, and extract its sequence number.
            let val = slot.load(Acquire);
            let seq = val >> SEQ_SHIFT;
            let dif = test_dbg!(seq as i8).wrapping_sub(test_dbg!(pos).wrapping_add(1) as i8);

            match test_dbg!(dif).cmp(&0) {
                cmp::Ordering::Less if test_dbg!(head & CLOSED) != 0 => {
                    if head & CLOSED_ERROR == CLOSED_ERROR {
                        return Err(TryRecvError::Error(unsafe { self.close_error() }));
                    } else {
                        return Err(TryRecvError::Closed);
                    }
                }
                cmp::Ordering::Less => return Err(TryRecvError::Empty),
                cmp::Ordering::Equal => match test_dbg!(self.dequeue_pos.compare_exchange_weak(
                    head,
                    head.wrapping_add(POS_ONE),
                    AcqRel,
                    Acquire,
                )) {
                    Ok(_) => {
                        slot.store(val.wrapping_add(SEQ_ONE), Release);
                        return Ok(Reservation {
                            core: self,
                            idx: (val & MASK) as u8,
                        });
                    }
                    Err(actual) => head = actual,
                },
                cmp::Ordering::Greater => head = test_dbg!(self.dequeue_pos.load(Acquire)),
            }
        }
    }

    fn commit_send(&self, idx: u8) -> Result<(), SendError<(), E>> {
        test_span!("Core::commit_send", idx);
        debug_assert!(idx as u16 <= MASK);
        let mut tail = test_dbg!(self.enqueue_pos.load(Acquire));
        loop {
            // Shift one bit to the right to extract the actual position, and
            // discard the `CLOSED` bit.
            let pos = tail >> POS_SHIFT;
            let slot = &self.queue[test_dbg!(pos & MASK) as usize];
            let seq = slot.load(Acquire) >> SEQ_SHIFT;
            let dif = test_dbg!(seq as i8).wrapping_sub(test_dbg!(pos as i8));

            match test_dbg!(dif).cmp(&0) {
                cmp::Ordering::Less => unreachable!(),
                cmp::Ordering::Equal => match test_dbg!(self.enqueue_pos.compare_exchange_weak(
                    tail,
                    tail.wrapping_add(POS_ONE),
                    AcqRel,
                    Acquire,
                )) {
                    Ok(_) => {
                        let new = test_dbg!(test_dbg!((pos) << SEQ_SHIFT).wrapping_add(SEQ_ONE));
                        slot.store(test_dbg!(idx as u16 | new), Release);
                        test_dbg!(self.cons_wait.wake());
                        return Ok(());
                    }
                    Err(actual) => tail = actual,
                },
                cmp::Ordering::Greater => tail = test_dbg!(self.enqueue_pos.load(Acquire)),
            }
        }
    }

    fn send_closed_error(&self) -> Option<E> {
        if test_dbg!(self.enqueue_pos.load(Acquire) & CLOSED_ERROR) == CLOSED_ERROR {
            Some(unsafe { self.close_error() })
        } else {
            None
        }
    }

    unsafe fn close_error(&self) -> E {
        // debug_assert!(self.enqueue_pos.load(Acquire) & CLOSED_ERROR == CLOSED_ERROR);
        self.error
            .with(|ptr| unsafe { (*ptr).assume_init_ref().clone() })
    }
}

unsafe impl<E: Send + Sync> Send for Core<E> {}
unsafe impl<E: Send + Sync> Sync for Core<E> {}

// === impl Reservation ===

impl<E: Clone> Reservation<'_, E> {
    pub(super) fn commit_send(self) -> Result<(), SendError<(), E>> {
        // don't run the destructor that frees the index, since we are dropping
        // the cell...
        let this = ManuallyDrop::new(self);
        // ...and commit to the queue.
        this.core.commit_send(this.idx)
    }
}

impl<E> Drop for Reservation<'_, E> {
    fn drop(&mut self) {
        unsafe { self.core.uncommit(self.idx) }
    }
}

impl<E> fmt::Debug for Reservation<'_, E> {
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

// == impl ErasedPipe ===

impl<E> ErasedPipe<E> {
    pub(super) unsafe fn new(ptr: *const (), vtable: &'static CoreVtable<E>) -> Self {
        Self { ptr, vtable }
    }

    /// # Safety
    ///
    /// This `ErasedPipe` must have been type-erased from a tricky-pipe with
    /// elements of type `T`!
    pub(super) unsafe fn typed<T>(self) -> TypedPipe<T, E> {
        TypedPipe {
            pipe: self,
            _t: PhantomData,
        }
    }

    pub(super) fn core(&self) -> &Core<E> {
        unsafe { &*(self.vtable.get_core)(self.ptr) }
    }

    pub(super) fn elems(&self) -> ErasedSlice {
        unsafe { (self.vtable.get_elems)(self.ptr) }
    }

    pub(super) fn fmt_into(&self, f: &mut fmt::DebugStruct<'_, '_>) -> fmt::Result {
        let Self { ptr, vtable } = self;
        f.field("ptr", &format_args!("{ptr:#p}"))
            .field("vtable", &format_args!("{vtable:#p}"))
            .field("type", &format_args!("{}", (vtable.type_name)()))
            .field("capacity", &self.core().capacity)
            .field("len", &self.core().len())
            .finish()
    }
}

impl<E> Clone for ErasedPipe<E> {
    fn clone(&self) -> Self {
        unsafe { (self.vtable.clone)(self.ptr) }
        Self {
            ptr: self.ptr,
            vtable: self.vtable,
        }
    }
}

impl<E> Drop for ErasedPipe<E> {
    fn drop(&mut self) {
        unsafe { (self.vtable.drop)(self.ptr) }
    }
}

// Safety: a pipe's element type must be `Send` in order to be erased.
unsafe impl<E: Send + Sync> Send for ErasedPipe<E> {}
// Safety: a pipe's element type must be `Send` in order to be erased.
unsafe impl<E: Send + Sync> Sync for ErasedPipe<E> {}

// === impl TypedPipe ===

impl<T: 'static, E> TypedPipe<T, E> {
    pub(super) fn core(&self) -> &Core<E> {
        self.pipe.core()
    }

    pub(super) fn elems(&self) -> &[UnsafeCell<MaybeUninit<T>>] {
        unsafe { self.pipe.elems().unerase::<UnsafeCell<MaybeUninit<T>>>() }
    }

    pub(super) fn fmt_into(&self, f: &mut fmt::DebugStruct<'_, '_>) -> fmt::Result {
        self.pipe.fmt_into(f)
    }
}

impl<T: 'static, E> Clone for TypedPipe<T, E> {
    fn clone(&self) -> Self {
        Self {
            pipe: self.pipe.clone(),
            _t: PhantomData,
        }
    }
}

unsafe impl<T: Send, E: Send + Sync> Send for TypedPipe<T, E> {}
unsafe impl<T: Send, E: Send + Sync> Sync for TypedPipe<T, E> {}

// === impl SerVtable ===

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

    pub(super) unsafe fn drop_elem<T: Serialize + 'static>(elems: ErasedSlice, idx: u8) {
        let elems = elems.unerase::<UnsafeCell<MaybeUninit<T>>>();
        elems[idx as usize].with_mut(|ptr| {
            let elem = (*ptr).as_mut_ptr();
            core::ptr::drop_in_place(elem)
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pos_bit_layout() {
        eprintln!("   CLOSED = {CLOSED:#016b}");
        eprintln!("  POS_ONE = {POS_ONE:#016b}");
        eprintln!("  SEQ_ONE = {SEQ_ONE:#016b}");
        eprintln!("     MASK = {MASK:#016b}");
        eprintln!("SEQ_SHIFT = {SEQ_SHIFT}");
        let packed_seq_bits = u16::BITS - (SEQ_SHIFT as u32 + 1);
        eprintln!(" seq bits = u16::BITS - (SEQ_SHIFT + 1) = {packed_seq_bits}");
        assert!(
            packed_seq_bits >= 2,
            "at least two bits (4 laps) should be used for sequence numbers"
        );
    }
}
