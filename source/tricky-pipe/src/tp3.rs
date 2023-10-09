use crate::loom::{
    cell::{self, UnsafeCell},
    sync::atomic::{self, AtomicU16, AtomicUsize, Ordering::*},
};
use core::{
    cmp, fmt,
    marker::PhantomData,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr,
};
use maitake_sync::{WaitCell, WaitQueue};
use mnemos_bitslab::index::IndexAllocWord;
use serde::{de::DeserializeOwned, Serialize};

const CAPACITY: usize = IndexAllocWord::CAPACITY as usize;
const SHIFT: usize = CAPACITY.trailing_zeros() as usize;
const SEQ_ONE: u16 = 1 << SHIFT;
const MASK: u16 = SEQ_ONE - 1;

#[cfg(not(test))]
macro_rules! dbg {
    ($x:expr) => {
        $x
    };
}

#[cfg(any(test, feature = "alloc"))]
mod arc_impl;
mod static_impl;

pub use self::static_impl::*;

#[cfg(any(test, feature = "alloc"))]
pub use self::arc_impl::*;

/// Values for the `core.state` bitfield.
mod state {
    /// If set, the channel's receiver has been claimed, indicating that no
    /// additional receivers can be claimed.
    pub(super) const RX_CLAIMED: usize = 1 << 1;

    /// If set, the channel's receiver has been dropped. This implies that the
    /// channel is closed by the receive side.
    pub(super) const RX_CLOSED: usize = 1 << 2;

    /// Sender reference count; value of one sender.
    pub(super) const TX_ONE: usize = 1 << 3;

    /// Mask for extracting sender reference count.
    pub(super) const TX_MASK: usize = !(RX_CLAIMED | RX_CLOSED);
}

struct Core {
    // TODO(eliza): should maybe cache-pad `dequeue_pos`` and `enqueue_pos` on
    // architectures with big cache lines...?
    //
    // or, we could lay the struct out so that other fields provide "free" padding...
    dequeue_pos: AtomicU16,
    enqueue_pos: AtomicU16,
    cons_wait: WaitCell,
    prod_wait: WaitQueue,
    indices: IndexAllocWord,
    queue: [AtomicU16; CAPACITY],
    /// Tracks the state of the the channel's senders/receivers, including
    /// whether a receiver has been claimed, whether the receiver has closed the
    /// channel (e.g. is dropped), and the number of active senders.
    state: AtomicUsize,
}

/// A type-erased slice.
#[derive(Copy, Clone)]
struct ErasedSlice {
    ptr: *const (),
    len: usize,
    #[cfg(debug_assertions)]
    typ: core::any::TypeId,
}

struct CoreVtable {
    get_core: unsafe fn(*const ()) -> *const Core,
    get_elems: unsafe fn(*const ()) -> ErasedSlice,
    clone: unsafe fn(*const ()),
    drop: unsafe fn(*const ()),
}

struct SerVtable {
    #[cfg(any(test, feature = "alloc"))]
    to_vec: SerVecFn,
    #[cfg(any(test, feature = "alloc"))]
    to_vec_framed: SerVecFn,
    to_slice: SerFn,
    to_slice_framed: SerFn,
}

struct DeserVtable {
    from_bytes: DeserFn,
    from_bytes_framed: DeserFn,
}

type SerFn = fn(ErasedSlice, u8, &mut [u8]) -> postcard::Result<&mut [u8]>;

#[cfg(any(test, feature = "alloc"))]
type SerVecFn = fn(ErasedSlice, u8) -> postcard::Result<Vec<u8>>;

type DeserFn = fn(ErasedSlice, u8, &[u8]) -> postcard::Result<()>;

impl ErasedSlice {
    fn erase<T: 'static>(slice: impl AsRef<[T]>) -> Self {
        let slice = slice.as_ref();
        let len = slice.len();
        #[cfg(debug_assertions)]
        let typ = core::any::TypeId::of::<T>();
        Self {
            ptr: slice.as_ptr().cast(),
            len,
            typ,
        }
    }

    unsafe fn unerase<'a, T: 'static>(self) -> &'a [T] {
        debug_assert_eq!(
            self.typ,
            core::any::TypeId::of::<T>(),
            "/!\\ EXTREMELY SERIOUS WARNING: you would have just done a type confusion, this is Real Bad"
        );
        core::slice::from_raw_parts(self.ptr.cast(), self.len)
    }
}

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

/// Erases both a pipe and its element type.
struct ErasedPipe {
    ptr: *const (),
    vtable: &'static CoreVtable,
}

struct TypedPipe<T: 'static> {
    pipe: ErasedPipe,
    _t: PhantomData<fn(T)>,
}

impl ErasedPipe {
    fn core(&self) -> &Core {
        unsafe { &*(self.vtable.get_core)(self.ptr) }
    }

    fn elems(&self) -> ErasedSlice {
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
    fn core(&self) -> &Core {
        self.pipe.core()
    }

    fn elems(&self) -> &[UnsafeCell<MaybeUninit<T>>] {
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

impl SerVtable {
    fn to_slice<T: Serialize + 'static>(
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

    fn to_slice_framed<T: Serialize + 'static>(
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
    fn to_vec<T: Serialize + 'static>(
        elems: ErasedSlice,
        idx: u8,
    ) -> postcard::Result<alloc::vec::Vec<u8>> {
        unsafe {
            let elems = elems.unerase::<UnsafeCell<MaybeUninit<T>>>();
            elems[idx as usize].with(|ptr| {
                let elem = (*ptr).assume_init_ref();
                postcard::to_allocvec(elem)
            })
        }
    }

    #[cfg(any(test, feature = "alloc"))]
    fn to_vec_framed<T: Serialize + 'static>(
        elems: ErasedSlice,
        idx: u8,
    ) -> postcard::Result<alloc::vec::Vec<u8>> {
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
    const fn new<T: DeserializeOwned + 'static>() -> Self {
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

impl Core {
    const fn new(max_capacity: u8) -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const QUEUE_INIT: AtomicU16 = AtomicU16::new(0);

        debug_assert!(max_capacity <= CAPACITY as u8);
        let mut queue = [QUEUE_INIT; CAPACITY];
        let mut i = 0;

        while i != CAPACITY {
            queue[i] = AtomicU16::new((i as u16) << SHIFT);
            i += 1;
        }

        Core {
            dequeue_pos: AtomicU16::new(0),
            enqueue_pos: AtomicU16::new(0),
            cons_wait: WaitCell::new(),
            prod_wait: WaitQueue::new(),
            indices: IndexAllocWord::new(),
            queue,
            state: AtomicUsize::new(0),
        }
    }

    fn try_reserve(&self) -> Result<Reservation<'_>, TrySendError> {
        if dbg!(self.state.load(Acquire)) & state::RX_CLOSED == state::RX_CLOSED {
            return Err(TrySendError::Closed);
        }
        self.indices
            .allocate()
            .ok_or(TrySendError::Full)
            .map(|idx| Reservation { core: self, idx })
    }

    async fn reserve(&self) -> Result<Reservation, SendError> {
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

    fn try_dequeue(&self) -> Result<Reservation<'_>, TryRecvError> {
        let mut pos = dbg!(self.dequeue_pos.load(Relaxed));
        loop {
            let slot = &self.queue[(pos & MASK) as usize];
            let val = dbg!(slot.load(Acquire));
            let seq = dbg!(val >> SHIFT);
            let dif = dbg!(seq as i8).wrapping_sub(pos.wrapping_add(1) as i8);

            match dbg!(dif).cmp(&0) {
                cmp::Ordering::Less => {
                    if dbg!(self.state.load(Acquire)) & state::TX_MASK == 0 {
                        return Err(TryRecvError::Closed);
                    } else {
                        return Err(TryRecvError::Empty);
                    }
                }
                cmp::Ordering::Equal => match dbg!(self.dequeue_pos.compare_exchange_weak(
                    pos,
                    pos.wrapping_add(1),
                    Relaxed,
                    Relaxed,
                )) {
                    Ok(_) => {
                        slot.store(val.wrapping_add(SEQ_ONE), Release);
                        return Ok(Reservation {
                            core: self,
                            idx: (val & MASK) as u8,
                        });
                    }
                    Err(actual) => pos = actual,
                },
                cmp::Ordering::Greater => pos = dbg!(self.dequeue_pos.load(Relaxed)),
            }
        }
    }

    fn commit_send(&self, idx: u8) {
        debug_assert!(dbg!(idx) as u16 <= MASK);
        let mut pos = dbg!(self.enqueue_pos.load(Relaxed));
        loop {
            let slot = &self.queue[dbg!(pos & MASK) as usize];
            let seq = dbg!(slot.load(Acquire)) >> SHIFT;
            let dif = dbg!(seq as i8).wrapping_sub(pos as i8);

            match dbg!(dif).cmp(&0) {
                cmp::Ordering::Less => unreachable!(),
                cmp::Ordering::Equal => match dbg!(self.enqueue_pos.compare_exchange_weak(
                    pos,
                    pos.wrapping_add(1),
                    Relaxed,
                    Relaxed,
                )) {
                    Ok(_) => {
                        let new = dbg!(dbg!(pos << SHIFT).wrapping_add(SEQ_ONE));
                        slot.store(dbg!(idx as u16 | new), Release);
                        self.cons_wait.wake();
                        return;
                    }
                    Err(actual) => pos = actual,
                },
                cmp::Ordering::Greater => pos = dbg!(self.enqueue_pos.load(Relaxed)),
            }
        }
    }

    unsafe fn uncommit(&self, idx: u8) {
        self.indices.free(idx);
        self.prod_wait.wake();
    }

    fn try_claim_rx(&self) -> Option<()> {
        // set `RX_CLAIMED`.
        let state = dbg!(self.state.fetch_or(state::RX_CLAIMED, AcqRel));
        // if the `RX_CLAIMED` bit was not set, we successfully claimed the
        // receiver.
        let claimed = dbg!(state & state::RX_CLAIMED) == 0;
        claimed.then_some(())
    }

    /// Close the channel from the receiver.
    fn close_rx(&self) {
        // set the state to indicate that the receiver was dropped.
        dbg!(self.state.fetch_or(state::RX_CLOSED, Release));
        // notify any waiting senders that the channel is closed.
        self.prod_wait.close();
    }

    #[inline]
    fn add_tx(&self) {
        dbg!(self.state.fetch_add(state::TX_ONE, Relaxed));
    }

    /// Drop a sender
    #[inline]
    fn drop_tx(&self) {
        let val = dbg!(self.state.fetch_sub(state::TX_ONE, Relaxed));
        if val & state::TX_MASK == state::TX_ONE {
            self.close_tx();
        }
    }

    #[inline(never)]
    fn close_tx(&self) {
        atomic::fence(Release);
        self.cons_wait.close();
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

// === impl Reservation ===
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

impl fmt::Debug for Reservation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { core, idx } = self;
        f.debug_struct("Reservation")
            .field("core", &format_args!("{core:#p}"))
            .field("idx", idx)
            .finish()
    }
}

/// Represents a closed error
#[derive(Debug, Eq, PartialEq)]
pub enum SendError {
    Closed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TrySendError {
    Full,
    Closed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TryRecvError {
    Empty,
    Closed,
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
    Send(TrySendError),
    Deserialize(postcard::Error),
}
