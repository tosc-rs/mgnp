use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    mem::{ManuallyDrop, MaybeUninit},
    sync::atomic::{AtomicU8, AtomicUsize},
};
use plugtail::{alloc::ArcPlugTail, Pluggable};

use maitake_sync::{WaitCell, WaitQueue};
use portable_atomic::Ordering;

const STATE_UNINIT: u8 = 0;
const STATE_INITING: u8 = 1;
const STATE_SPLIT: u8 = 3;
// TODO: refcount access? some kind of ctr? Some way of handling
// re-opening cases? Use fancy mycelium bitfield?
const STATE_CLOSED: u8 = 4;

mod sealed {
    use plugtail::BodyDrop;

    use super::*;

    pub struct TPHdr<T> {
        pub(crate) dequeue_pos: AtomicUsize,
        pub(crate) enqueue_pos: AtomicUsize,
        pub(crate) cons_wait: WaitCell,
        pub(crate) prod_wait: WaitQueue,
        pub(crate) state: AtomicU8,
        pub(crate) pd: PhantomData<T>,
    }

    impl<T> TPHdr<T> {
        pub(crate) fn new_init() -> Self {
            Self {
                dequeue_pos: AtomicUsize::new(0),
                enqueue_pos: AtomicUsize::new(0),
                cons_wait: WaitCell::new(),
                prod_wait: WaitQueue::new(),
                state: AtomicU8::new(STATE_SPLIT),
                pd: PhantomData,
            }
        }
    }

    impl<T> BodyDrop for TPHdr<T> {
        type Item = QCell<T>;

        fn body_drop(&self, i: &[core::cell::UnsafeCell<core::mem::MaybeUninit<Self::Item>>]) {
            todo!()
        }
    }
}

#[derive(Clone)]
struct TrickyPipe<T, P>
where
    T: 'static,
    P: Pluggable<Header = sealed::TPHdr<T>, Item = QCell<T>>,
{
    p: P,
}

impl<T, P> TrickyPipe<T, P>
where
    T: 'static,
    P: Pluggable<Header = sealed::TPHdr<T>, Item = QCell<T>>,
{
    /// Returns the item in the front of the queue, or `None` if the queue is empty
    fn dequeue_sync(&self) -> Option<T> {
        let sto = self.p.storage();
        // Note: DON'T check the closed flag on dequeue. We want to be able
        // to drain any potential messages after closing.
        let (ptr, len) = {
            let len = sto.t.len();
            let ptr: *const UnsafeCell<MaybeUninit<QCell<T>>> = sto.t.as_ptr();
            let ptr: *const QCell<T> = ptr.cast();
            (ptr.cast_mut(), len)
        };
        let res = unsafe { dequeue(ptr, &sto.hdr.dequeue_pos, len - 1) };
        if res.is_some() {
            sto.hdr.prod_wait.wake_all();
        }
        res
    }

    async fn dequeue_async(&self) -> Result<T, DequeueError> {
        let sto = self.p.storage();
        loop {
            // note: this future always completes immediately, it's awaited just
            // to grab the wait future with a preregistered waker.
            let wait = sto.hdr.cons_wait.subscribe().await;
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
    fn enqueue_sync(&self, item: T) -> Result<(), EnqueueError<T>> {
        let sto = self.p.storage();
        match sto.hdr.state.load(Ordering::Acquire) {
            STATE_SPLIT => {}
            STATE_CLOSED => return Err(EnqueueError::Closed(item)),
            _ => return Err(EnqueueError::InternalError(item)),
        }
        let (ptr, len) = {
            let len = sto.t.len();
            let ptr: *const UnsafeCell<MaybeUninit<QCell<T>>> = sto.t.as_ptr();
            let ptr: *const QCell<T> = ptr.cast();
            (ptr.cast_mut(), len)
        };
        let res = unsafe { enqueue(ptr, &sto.hdr.enqueue_pos, len - 1, item) };
        if res.is_ok() {
            sto.hdr.cons_wait.wake();
        }
        res.map_err(EnqueueError::Full)
    }

    async fn enqueue_async(&self, mut item: T) -> Result<(), EnqueueError<T>> {
        let sto = self.p.storage();
        loop {
            match self.enqueue_sync(item) {
                // We succeeded or the queue is closed, propagate those errors
                ok @ Ok(_) => return ok,
                err @ Err(EnqueueError::Closed(_)) | err @ Err(EnqueueError::InternalError(_)) => {
                    return err
                }

                // It's full, let's wait until it isn't or the channel has closed
                Err(EnqueueError::Full(eitem)) => {
                    match sto.hdr.prod_wait.wait().await {
                        Ok(()) => {}
                        Err(_) => return Err(EnqueueError::Closed(eitem)),
                    }
                    item = eitem;
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct Sender<T, P>
where
    T: 'static,
    P: Pluggable<Header = sealed::TPHdr<T>, Item = QCell<T>>,
{
    pt: TrickyPipe<T, P>,
}

#[derive(Clone)]
pub struct Receiver<T, P>
where
    T: 'static,
    P: Pluggable<Header = sealed::TPHdr<T>, Item = QCell<T>>,
{
    pt: TrickyPipe<T, P>,
}

unsafe fn type_drop_sender<T, P>(container: *const ())
where
    T: 'static,
    P: Pluggable<Header = sealed::TPHdr<T>, Item = QCell<T>>,
{
    P::unleak(container);
}

pub struct RefSender {
    leaked_pluggable: *const (),
    d: unsafe fn(*const ()),
}

impl RefSender {
    fn erase<T, P>(s: Sender<T, P>) -> RefSender
    where
        T: 'static,
        P: Pluggable<Header = sealed::TPHdr<T>, Item = QCell<T>>,
    {
        let leaked_pluggable = s.pt.p.leak();
        Self {
            leaked_pluggable,
            d: type_drop_sender::<T, P>,
        }
    }
}

impl Drop for RefSender {
    fn drop(&mut self) {
        unsafe {
            (self.d)(self.leaked_pluggable);
        }
    }
}

impl<T, P> Sender<T, P>
where
    T: 'static,
    P: Pluggable<Header = sealed::TPHdr<T>, Item = QCell<T>>,
{
    /// Adds an `item` to the end of the queue
    ///
    /// Returns back the `item` if the queue is full
    #[inline]
    pub fn enqueue_sync(&self, item: T) -> Result<(), EnqueueError<T>> {
        self.pt.enqueue_sync(item)
    }

    #[inline]
    pub async fn enqueue_async(&self, item: T) -> Result<(), EnqueueError<T>> {
        self.pt.enqueue_async(item).await
    }
}

impl<T, P> Receiver<T, P>
where
    T: 'static,
    P: Pluggable<Header = sealed::TPHdr<T>, Item = QCell<T>>,
{
    /// Returns the item in the front of the queue, or `None` if the queue is empty
    pub fn dequeue_sync(&self) -> Option<T> {
        self.pt.dequeue_sync()
    }

    pub async fn dequeue_async(&self) -> Result<T, DequeueError> {
        self.pt.dequeue_async().await
    }
}

pub fn arc_channel<T>(
    n: usize,
) -> (
    Sender<T, ArcPlugTail<sealed::TPHdr<T>, QCell<T>>>,
    Receiver<T, ArcPlugTail<sealed::TPHdr<T>, QCell<T>>>,
)
where
    T: 'static,
{
    let tp = TrickyPipe {
        p: ArcPlugTail::new(
            sealed::TPHdr::new_init(),
            n,
        ),
    };

    let sto = tp.p.storage();
    sto.t.iter().enumerate().for_each(|(i, slot)| unsafe {
        slot.get().write(MaybeUninit::new(QCell {
            data: MaybeUninit::uninit(),
            sequence: AtomicUsize::new(i),
        }));
    });

    (Sender { pt: TrickyPipe { p: tp.p.clone() } }, Receiver { pt: tp })
}

pub fn arc_reftx_channel<T>(
    n: usize,
) -> (
    RefSender,
    Receiver<T, ArcPlugTail<sealed::TPHdr<T>, QCell<T>>>,
)
where
    T: 'static,
{
    let tp = TrickyPipe {
        p: ArcPlugTail::new(
            sealed::TPHdr::new_init(),
            n,
        ),
    };

    let sto = tp.p.storage();
    sto.t.iter().enumerate().for_each(|(i, slot)| unsafe {
        slot.get().write(MaybeUninit::new(QCell {
            data: MaybeUninit::uninit(),
            sequence: AtomicUsize::new(i),
        }));
    });

    let tx = Sender { pt: TrickyPipe { p: tp.p.clone() } };
    let tx = RefSender::erase(tx);

    (tx, Receiver { pt: tp })
}

unsafe fn dequeue<T>(buffer: *mut QCell<T>, dequeue_pos: &AtomicUsize, mask: usize) -> Option<T> {
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
    buffer: *mut QCell<T>,
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

pub struct QCell<T> {
    data: MaybeUninit<T>,
    sequence: AtomicUsize,
}

pub const fn single_cell<T>() -> QCell<T> {
    QCell {
        data: MaybeUninit::uninit(),
        sequence: AtomicUsize::new(0),
    }
}

pub fn cell_array<const N: usize, T: Sized>() -> [QCell<T>; N] {
    [QCell::<T>::SINGLE_CELL; N]
}

impl<T> QCell<T> {
    const SINGLE_CELL: Self = Self::new(0);

    const fn new(seq: usize) -> Self {
        Self {
            data: MaybeUninit::uninit(),
            sequence: AtomicUsize::new(seq),
        }
    }
}

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
