use crate::loom::{
    cell::{self, UnsafeCell},
    sync::atomic::{self, AtomicUsize, Ordering::*},
};
use core::{
    cmp, fmt,
    mem::{ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
};
use maitake_sync::{WaitCell, WaitQueue};
use mnemos_bitslab::index::IndexAllocWord;
use serde::{de::DeserializeOwned, Serialize};

const CAPACITY: usize = IndexAllocWord::CAPACITY as usize;
const SHIFT: usize = CAPACITY.trailing_zeros() as usize;
const SEQ_ONE: usize = 1 << SHIFT;
const MASK: usize = SEQ_ONE - 1;

#[cfg(not(test))]
macro_rules! dbg {
    ($x:expr) => {
        $x
    };
}

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
    /// Tracks the state of the the channel's senders/receivers, including
    /// whether a receiver has been claimed, whether the receiver has closed the
    /// channel (e.g. is dropped), and the number of active senders.
    state: AtomicUsize,
}

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

impl<T> TrickyPipe<T> {
    const EMPTY_CELL: UnsafeCell<MaybeUninit<T>> = UnsafeCell::new(MaybeUninit::uninit());
    #[allow(clippy::declare_interior_mutable_const)]
    const QUEUE_INIT: AtomicUsize = AtomicUsize::new(0);

    pub const fn new() -> Self {
        let mut queue = [Self::QUEUE_INIT; CAPACITY];
        let mut i = 0;

        while i != CAPACITY {
            queue[i] = AtomicUsize::new(i << SHIFT);
            i += 1;
        }
        Self {
            core: Core {
                dequeue_pos: AtomicUsize::new(0),
                enqueue_pos: AtomicUsize::new(0),
                cons_wait: WaitCell::new(),
                prod_wait: WaitQueue::new(),
                indices: IndexAllocWord::new(),
                queue,
                state: AtomicUsize::new(0),
            },
            elements: [Self::EMPTY_CELL; CAPACITY],
        }
    }

    pub fn receiver(&self) -> Option<Receiver<'_, T>> {
        self.core.try_claim_rx()?;

        Some(Receiver { pipe: self })
    }

    pub fn sender(&self) -> Sender<'_, T> {
        self.core.add_tx();
        Sender { pipe: self }
    }
}

impl<T: Serialize> TrickyPipe<T> {
    pub fn ser_receiver(&self) -> Option<SerReceiver<'_>> {
        self.core.try_claim_rx()?;

        Some(SerReceiver {
            core: &self.core,
            elems: self.elements.as_ptr() as *const (),
            vtable: Self::SER_VTABLE,
        })
    }

    const SER_VTABLE: &'static SerVtable = &SerVtable {
        #[cfg(any(test, feature = "alloc"))]
        to_vec: Self::to_vec,
        #[cfg(any(test, feature = "alloc"))]
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

    #[cfg(any(test, feature = "alloc"))]
    fn to_vec(elems: *const (), idx: u8) -> postcard::Result<alloc::vec::Vec<u8>> {
        unsafe {
            let elems = elems as *const UnsafeCell<MaybeUninit<T>>;
            // TODO(eliza): since this is unsafe anyway, we *could* just do
            // pointer math and elide the bounds check... &shrug;
            let elems = core::slice::from_raw_parts(elems, CAPACITY);
            elems[idx as usize].with(|ptr| {
                let elem = (*ptr).assume_init_ref();
                postcard::to_allocvec(elem)
            })
        }
    }

    #[cfg(any(test, feature = "alloc"))]
    fn to_vec_framed(elems: *const (), idx: u8) -> postcard::Result<alloc::vec::Vec<u8>> {
        unsafe {
            let elems = elems as *const UnsafeCell<MaybeUninit<T>>;
            // TODO(eliza): since this is unsafe anyway, we *could* just do
            // pointer math and elide the bounds check... &shrug;
            let elems = core::slice::from_raw_parts(elems, CAPACITY);
            elems[idx as usize].with(|ptr| {
                let elem = (*ptr).assume_init_ref();
                postcard::to_allocvec_cobs(elem)
            })
        }
    }
}

impl<T: DeserializeOwned> TrickyPipe<T> {
    pub fn ser_sender(&self) -> SerSender<'_> {
        self.core.add_tx();
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

type SerFn = fn(*const (), u8, &mut [u8]) -> postcard::Result<&mut [u8]>;

#[cfg(any(test, feature = "alloc"))]
type SerVecFn = fn(*const (), u8) -> postcard::Result<Vec<u8>>;

type DeserFn = fn(*const (), u8, &[u8]) -> postcard::Result<()>;

// === impl Receiver ===

impl<T> Receiver<'_, T> {
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let res = self.pipe.core.try_dequeue()?;
        let elem =
            self.pipe.elements[res.idx as usize].with(|ptr| unsafe { (*ptr).assume_init_read() });
        Ok(elem)
    }

    pub async fn recv(&self) -> Result<T, RecvError> {
        loop {
            match self.try_recv() {
                Ok(e) => return Ok(e),
                Err(TryRecvError::Closed) => return Err(RecvError::Closed),
                Err(TryRecvError::Empty) => self
                    .pipe
                    .core
                    .cons_wait
                    .wait()
                    .await
                    .map_err(|_| RecvError::Closed)?,
            }
        }
    }
}

impl<T> Drop for Receiver<'_, T> {
    fn drop(&mut self) {
        self.pipe.core.close_rx();
    }
}

// === impl SerReceiver ===

impl SerReceiver<'_> {
    /// Attempt to receive a message from the channel, if there are currently
    /// any messages in the channel.
    ///
    /// This method returns a [`SerRecvRef`] which may be used to serialize the
    /// message.
    pub fn try_recv(&self) -> Result<SerRecvRef<'_>, TryRecvError> {
        let res = self.core.try_dequeue()?;
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
                Ok(res) => break res,
                Err(TryRecvError::Closed) => return Err(RecvError::Closed),
                Err(TryRecvError::Empty) => self
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

impl Drop for SerReceiver<'_> {
    fn drop(&mut self) {
        self.core.close_rx();
    }
}

// === impl SerRecvRef ===

impl SerRecvRef<'_> {
    pub fn to_slice<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        (self.vtable.to_slice)(self.elems, self.pipe.idx, buf)
    }

    pub fn to_slice_framed<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        (self.vtable.to_slice_framed)(self.elems, self.pipe.idx, buf)
    }

    /// Serializes the message to an owned `Vec`.
    #[cfg(any(test, feature = "alloc"))]
    pub fn to_vec(&self) -> postcard::Result<alloc::vec::Vec<u8>> {
        (self.vtable.to_vec)(self.elems, self.pipe.idx)
    }

    /// Returns the serialized representation of the message as a COBS frame, in
    /// an owned `Vec`.
    #[cfg(any(test, feature = "alloc"))]
    pub fn to_vec_framed(&self) -> postcard::Result<alloc::vec::Vec<u8>> {
        (self.vtable.to_vec_framed)(self.elems, self.pipe.idx)
    }
}

impl fmt::Debug for SerRecvRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            pipe,
            elems: _,
            vtable,
        } = self;
        f.debug_struct("SerRecvRef")
            .field("pipe", &pipe)
            .field("vtable", &format_args!("{vtable:p}"))
            .finish()
    }
}

// === impl SerSender ===

impl SerSender<'_> {
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
            match self.core.try_reserve() {
                Ok(res) => {
                    // try writing the bytes to the reservation.
                    deserialize(self.elems, res.idx, bytes).map_err(SerSendError::Deserialize)?;
                    // if we successfully deserialized the bytes, commit the send.
                    // otherwise, we'll release the send index when we drop the reservation.
                    res.commit_send();
                    return Ok(());
                }
                Err(TrySendError::Closed) => return Err(SerSendError::Closed),
                Err(TrySendError::Full) => self
                    .core
                    .prod_wait
                    .wait()
                    .await
                    .map_err(|_| SerSendError::Closed)?,
            }
        }
    }

    fn try_send_inner(&self, bytes: &[u8], deserialize: DeserFn) -> Result<(), SerTrySendError> {
        let res = self.core.try_reserve().map_err(SerTrySendError::Send)?;
        // try writing the bytes to the reservation.
        deserialize(self.elems, res.idx, bytes).map_err(SerTrySendError::Deserialize)?;
        // if we successfully deserialized the bytes, commit the send.
        // otherwise, we'll release the send index when we drop the reservation.
        res.commit_send();
        Ok(())
    }
}

impl Clone for SerSender<'_> {
    fn clone(&self) -> Self {
        self.core.add_tx();
        Self {
            core: self.core,
            elems: self.elems,
            vtable: self.vtable,
        }
    }
}

impl Drop for SerSender<'_> {
    fn drop(&mut self) {
        self.core.drop_tx();
    }
}

// === impl Sender ===

impl<T> Sender<'_, T> {
    pub fn try_reserve(&self) -> Result<SendRef<'_, T>, TrySendError> {
        let pipe = self.pipe.core.try_reserve()?;
        let cell = self.pipe.elements[pipe.idx as usize].get_mut();
        Ok(SendRef { cell, pipe })
    }

    pub async fn reserve(&self) -> Result<SendRef<'_, T>, SendError> {
        let pipe = self.pipe.core.reserve().await?;
        let cell = self.pipe.elements[pipe.idx as usize].get_mut();
        Ok(SendRef { cell, pipe })
    }
}

impl<T> Clone for Sender<'_, T> {
    fn clone(&self) -> Self {
        self.pipe.core.add_tx();
        Self { pipe: self.pipe }
    }
}

impl<T> Drop for Sender<'_, T> {
    fn drop(&mut self) {
        self.pipe.core.drop_tx();
    }
}

impl Core {
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
            let slot = &self.queue[pos & MASK];
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
        debug_assert!(dbg!(idx) as usize <= MASK);
        let mut pos = dbg!(self.enqueue_pos.load(Relaxed));
        loop {
            let slot = &self.queue[dbg!(pos & MASK)];
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
                        slot.store(dbg!(idx as usize | new), Release);
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

#[cfg(test)]
mod test {

    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Serialize, PartialEq)]
    struct SerStruct {
        a: u8,
        b: i16,
        c: u32,
        d: String,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct DeStruct {
        a: u8,
        b: i16,
        c: u32,
        d: String,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct SerDeStruct {
        a: u8,
        b: i16,
        c: u32,
        d: String,
    }

    #[derive(Debug, PartialEq)]
    struct UnSerStruct {
        a: u8,
        b: i16,
        c: u32,
        d: String,
    }

    #[test]
    fn normal_smoke() {
        let chan = TrickyPipe::<UnSerStruct>::new();
        let tx = chan.sender();
        let rx = chan.receiver().unwrap();
        tx.try_reserve().unwrap().send(UnSerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        });

        tx.try_reserve().unwrap().send(UnSerStruct {
            a: 20,
            b: -8000,
            c: 200_000,
            d: String::from("greets"),
        });

        tx.try_reserve().unwrap().send(UnSerStruct {
            a: 100,
            b: -1_000,
            c: 300_000,
            d: String::from("oh my"),
        });

        assert_eq!(
            rx.try_recv().unwrap(),
            UnSerStruct {
                a: 240,
                b: -6_000,
                c: 100_000,
                d: String::from("hello"),
            }
        );

        assert_eq!(
            rx.try_recv().unwrap(),
            UnSerStruct {
                a: 20,
                b: -8000,
                c: 200_000,
                d: String::from("greets"),
            }
        );

        assert_eq!(
            rx.try_recv().unwrap(),
            UnSerStruct {
                a: 100,
                b: -1_000,
                c: 300_000,
                d: String::from("oh my"),
            }
        );
    }

    #[test]
    fn normal_closed_rx() {
        let chan = TrickyPipe::<UnSerStruct>::new();
        let tx = chan.sender();
        let rx = chan.receiver().unwrap();
        drop(rx);
        assert_eq!(tx.try_reserve().unwrap_err(), TrySendError::Closed,);
    }

    #[test]
    fn normal_closed_tx() {
        let chan = TrickyPipe::<UnSerStruct>::new();
        let tx = chan.sender();
        let rx = chan.receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed,);
    }

    #[test]
    fn normal_closed_cloned_tx() {
        let chan = TrickyPipe::<UnSerStruct>::new();
        let tx1 = chan.sender();
        let tx2 = tx1.clone();
        let rx = chan.receiver().unwrap();
        drop(tx1);
        drop(tx2);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed,);
    }

    #[test]
    fn ser_smoke() {
        let chan = TrickyPipe::<SerStruct>::new();
        let tx = chan.sender();
        let rx = chan.ser_receiver().unwrap();
        tx.try_reserve().unwrap().send(SerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        });

        tx.try_reserve().unwrap().send(SerStruct {
            a: 20,
            b: -8000,
            c: 200_000,
            d: String::from("greets"),
        });

        tx.try_reserve().unwrap().send(SerStruct {
            a: 100,
            b: -1_000,
            c: 300_000,
            d: String::from("oh my"),
        });

        assert_eq!(
            rx.try_recv().unwrap().to_vec(),
            Ok(vec![240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111])
        );

        assert_eq!(
            rx.try_recv().unwrap().to_vec(),
            Ok(vec![
                20, 255, 124, 192, 154, 12, 6, 103, 114, 101, 101, 116, 115
            ])
        );

        assert_eq!(
            rx.try_recv().unwrap().to_vec(),
            Ok(vec![100, 207, 15, 224, 167, 18, 5, 111, 104, 32, 109, 121])
        );
    }

    #[test]
    fn ser_closed_rx() {
        let chan = TrickyPipe::<SerStruct>::new();
        let tx = chan.sender();
        let rx = chan.ser_receiver().unwrap();
        drop(rx);
        assert_eq!(tx.try_reserve().unwrap_err(), TrySendError::Closed);
    }

    #[test]
    fn ser_closed_tx() {
        let chan = TrickyPipe::<SerStruct>::new();
        let tx = chan.sender();
        let rx = chan.ser_receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    }

    #[test]
    fn ser_closed_cloned_tx() {
        let chan = TrickyPipe::<SerStruct>::new();
        let tx1 = chan.sender();
        let tx2 = tx1.clone();
        let rx = chan.ser_receiver().unwrap();
        drop(tx1);
        drop(tx2);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    }

    #[test]
    fn ser_ref_smoke() {
        let chan = TrickyPipe::<SerStruct>::new();
        let tx = chan.sender();
        let rx = chan.ser_receiver().unwrap();
        tx.try_reserve().unwrap().send(SerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        });

        tx.try_reserve().unwrap().send(SerStruct {
            a: 20,
            b: -8000,
            c: 200_000,
            d: String::from("greets"),
        });

        tx.try_reserve().unwrap().send(SerStruct {
            a: 100,
            b: -1_000,
            c: 300_000,
            d: String::from("oh my"),
        });

        let mut buf = [0u8; 128];

        assert_eq!(
            rx.try_recv().unwrap().to_slice(&mut buf).unwrap(),
            &mut [240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]
        );

        assert_eq!(
            rx.try_recv().unwrap().to_slice(&mut buf).unwrap(),
            &mut [20, 255, 124, 192, 154, 12, 6, 103, 114, 101, 101, 116, 115]
        );

        assert_eq!(
            rx.try_recv().unwrap().to_slice(&mut buf).unwrap(),
            &mut [100, 207, 15, 224, 167, 18, 5, 111, 104, 32, 109, 121]
        );
    }

    #[test]
    fn deser_smoke() {
        let chan = TrickyPipe::<DeStruct>::new();
        let tx = chan.ser_sender();
        let rx = chan.receiver().unwrap();
        tx.try_send([240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111])
            .unwrap();

        tx.try_send([20, 255, 124, 192, 154, 12, 6, 103, 114, 101, 101, 116, 115])
            .unwrap();

        tx.try_send([100, 207, 15, 224, 167, 18, 5, 111, 104, 32, 109, 121])
            .unwrap();

        assert_eq!(
            rx.try_recv(),
            Ok(DeStruct {
                a: 240,
                b: -6_000,
                c: 100_000,
                d: String::from("hello"),
            })
        );

        assert_eq!(
            rx.try_recv(),
            Ok(DeStruct {
                a: 20,
                b: -8000,
                c: 200_000,
                d: String::from("greets"),
            })
        );

        assert_eq!(
            rx.try_recv(),
            Ok(DeStruct {
                a: 100,
                b: -1_000,
                c: 300_000,
                d: String::from("oh my"),
            })
        );
    }

    #[test]
    fn deser_closed_rx() {
        let chan = TrickyPipe::<DeStruct>::new();
        let tx = chan.ser_sender();
        let rx = chan.receiver().unwrap();
        drop(rx);
        let res = tx.try_send([240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]);
        assert_eq!(
            res.unwrap_err(),
            SerTrySendError::Send(TrySendError::Closed),
        );
    }

    #[test]
    fn deser_closed_tx() {
        let chan = TrickyPipe::<DeStruct>::new();
        let tx = chan.ser_sender();
        let rx = chan.receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    }

    #[test]
    fn deser_closed_cloned_tx() {
        let chan = TrickyPipe::<DeStruct>::new();
        let tx1 = chan.ser_sender();
        let tx2 = tx1.clone();
        let rx = chan.receiver().unwrap();
        drop(tx1);
        drop(tx2);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    }

    #[test]
    fn deser_ser_smoke() {
        // Ideally the "serialize on both sides" case would just be a BBQueue or
        // some other kind of framed byte pipe thingy, but we should make sure
        // it works anyway i guess...
        let chan = TrickyPipe::<SerDeStruct>::new();
        let tx = chan.ser_sender();
        let rx = chan.ser_receiver().unwrap();
        const MSG_ONE: &[u8] = &[240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111];
        const MSG_TWO: &[u8] = &[20, 255, 124, 192, 154, 12, 6, 103, 114, 101, 101, 116, 115];
        const MSG_THREE: &[u8] = &[100, 207, 15, 224, 167, 18, 5, 111, 104, 32, 109, 121];
        tx.try_send(MSG_ONE).unwrap();

        tx.try_send(MSG_TWO).unwrap();

        tx.try_send(MSG_THREE).unwrap();

        let mut buf = [0u8; 128];

        assert_eq!(rx.try_recv().unwrap().to_slice(&mut buf).unwrap(), MSG_ONE);

        assert_eq!(rx.try_recv().unwrap().to_slice(&mut buf).unwrap(), MSG_TWO);

        assert_eq!(
            rx.try_recv().unwrap().to_slice(&mut buf).unwrap(),
            MSG_THREE
        );
    }
}
