use core::marker::PhantomData;
use core::mem::{transmute, ManuallyDrop, MaybeUninit};
use core::ops::Deref;

// TODO
use std::sync::mpsc;

use serde::de::DeserializeOwned;
use serde::Serialize;

// TODO: only pub to silence unused warnings
pub mod spitebuf;

enum Never {}

enum SendOutcome {
    Taken,
    Untaken,
}

enum SendRefOutcome {
    Success,
    Failure,
}

enum RecvOutcome {
    Given,
    Ungiven,
}

type SendFunc = fn(*const mpsc::SyncSender<Never>, *const Never) -> SendOutcome;
type SendRefFunc = fn(*const mpsc::SyncSender<Never>, &[u8]) -> SendRefOutcome;
type RecvFunc = fn(*const mpsc::Receiver<Never>, *mut Never) -> RecvOutcome;
type RecvRefFunc = fn(*const mpsc::Receiver<Never>, &mut [u8]) -> Result<&mut [u8], ()>;
type SenderDropFunc = unsafe fn(&mut ManuallyDrop<mpsc::SyncSender<Never>>);
type ReceiverDropFunc = unsafe fn(&mut ManuallyDrop<mpsc::Receiver<Never>>);

pub struct Sender<T> {
    _pd: PhantomData<T>,
    tx: ManuallyDrop<mpsc::SyncSender<Never>>,
    f: SendFunc,
    d: SenderDropFunc,
}

impl<T> Sender<T> {
    pub fn send(&self, t: T) -> Result<(), T> {
        let inbox = MaybeUninit::new(t);

        match (self.f)(self.tx.deref(), inbox.as_ptr().cast()) {
            SendOutcome::Taken => Ok(()),
            SendOutcome::Untaken => Err(unsafe { inbox.assume_init() }),
        }
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        unsafe { (self.d)(&mut self.tx) }
    }
}

pub struct RefSender {
    tx: ManuallyDrop<mpsc::SyncSender<Never>>,
    f: SendRefFunc,
    d: SenderDropFunc,
}

impl RefSender {
    pub fn send_slice(&self, sli: &[u8]) -> Result<(), ()> {
        match (self.f)(self.tx.deref(), sli) {
            SendRefOutcome::Success => Ok(()),
            SendRefOutcome::Failure => Err(()),
        }
    }
}

impl Drop for RefSender {
    fn drop(&mut self) {
        unsafe { (self.d)(&mut self.tx) }
    }
}

pub struct Receiver<T> {
    _pd: PhantomData<T>,
    rx: ManuallyDrop<mpsc::Receiver<Never>>,
    f: RecvFunc,
    d: ReceiverDropFunc,
}

#[derive(Debug, PartialEq)]
pub enum RecvError {
    Oops,
}

impl<T> Receiver<T> {
    pub fn recv(&self) -> Result<T, RecvError> {
        let mut outbox: MaybeUninit<T> = MaybeUninit::uninit();
        let outref: *mut T = outbox.as_mut_ptr();
        let outref: *mut Never = outref.cast();

        match (self.f)(self.rx.deref(), outref) {
            RecvOutcome::Given => Ok(unsafe { outbox.assume_init() }),
            RecvOutcome::Ungiven => Err(RecvError::Oops),
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        unsafe { (self.d)(&mut self.rx) }
    }
}

pub struct RefReceiver {
    rx: ManuallyDrop<mpsc::Receiver<Never>>,
    f: RecvRefFunc,
    d: ReceiverDropFunc,
}

impl RefReceiver {
    pub fn recv_slice<'a>(&self, sli: &'a mut [u8]) -> Result<&'a mut [u8], ()> {
        (self.f)(self.rx.deref(), sli)
    }
}

impl Drop for RefReceiver {
    fn drop(&mut self) {
        unsafe { (self.d)(&mut self.rx) }
    }
}

pub fn channel<T>(bound: usize) -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::sync_channel::<T>(bound);
    let tx: ManuallyDrop<mpsc::SyncSender<Never>> = unsafe { transmute(ManuallyDrop::new(tx)) };
    let rx: ManuallyDrop<mpsc::Receiver<Never>> = unsafe { transmute(ManuallyDrop::new(rx)) };

    let tx: Sender<T> = Sender {
        _pd: PhantomData,
        tx,
        f: bypass_sender::<T>,
        d: type_drop_sender::<T>,
    };

    let rx: Receiver<T> = Receiver {
        _pd: PhantomData,
        rx,
        f: bypass_receiver::<T>,
        d: type_drop_receiver::<T>,
    };

    (tx, rx)
}

pub fn ser_channel<T: Serialize>(bound: usize) -> (Sender<T>, Receiver<Vec<u8>>) {
    let (tx, rx) = mpsc::sync_channel::<T>(bound);
    let tx: ManuallyDrop<mpsc::SyncSender<Never>> = unsafe { transmute(ManuallyDrop::new(tx)) };
    let rx: ManuallyDrop<mpsc::Receiver<Never>> = unsafe { transmute(ManuallyDrop::new(rx)) };

    let tx: Sender<T> = Sender {
        _pd: PhantomData,
        tx,
        f: bypass_sender::<T>,
        d: type_drop_sender::<T>,
    };

    let rx: Receiver<Vec<u8>> = Receiver {
        _pd: PhantomData,
        rx,
        f: ser_receiver::<T>,
        d: type_drop_receiver::<T>,
    };

    (tx, rx)
}

pub fn ser_ref_channel<T: Serialize>(bound: usize) -> (Sender<T>, RefReceiver) {
    let (tx, rx) = mpsc::sync_channel::<T>(bound);
    let tx: ManuallyDrop<mpsc::SyncSender<Never>> = unsafe { transmute(ManuallyDrop::new(tx)) };
    let rx: ManuallyDrop<mpsc::Receiver<Never>> = unsafe { transmute(ManuallyDrop::new(rx)) };

    let tx: Sender<T> = Sender {
        _pd: PhantomData,
        tx,
        f: bypass_sender::<T>,
        d: type_drop_sender::<T>,
    };

    let rx: RefReceiver = RefReceiver {
        rx,
        f: ser_ref_receiver::<T>,
        d: type_drop_receiver::<T>,
    };

    (tx, rx)
}

pub fn deser_channel<T: DeserializeOwned>(bound: usize) -> (Sender<Vec<u8>>, Receiver<T>) {
    let (tx, rx) = mpsc::sync_channel::<T>(bound);
    let tx: ManuallyDrop<mpsc::SyncSender<Never>> = unsafe { transmute(ManuallyDrop::new(tx)) };
    let rx: ManuallyDrop<mpsc::Receiver<Never>> = unsafe { transmute(ManuallyDrop::new(rx)) };

    let tx: Sender<Vec<u8>> = Sender {
        _pd: PhantomData,
        tx,
        f: deser_sender::<T>,
        d: type_drop_sender::<T>,
    };

    let rx: Receiver<T> = Receiver {
        _pd: PhantomData,
        rx,
        f: bypass_receiver::<T>,
        d: type_drop_receiver::<T>,
    };

    (tx, rx)
}

pub fn deser_ref_channel<T: DeserializeOwned>(bound: usize) -> (RefSender, Receiver<T>) {
    let (tx, rx) = mpsc::sync_channel::<T>(bound);
    let tx: ManuallyDrop<mpsc::SyncSender<Never>> = unsafe { transmute(ManuallyDrop::new(tx)) };
    let rx: ManuallyDrop<mpsc::Receiver<Never>> = unsafe { transmute(ManuallyDrop::new(rx)) };

    let tx: RefSender = RefSender {
        tx,
        f: deser_ref_sender::<T>,
        d: type_drop_sender::<T>,
    };

    let rx: Receiver<T> = Receiver {
        _pd: PhantomData,
        rx,
        f: bypass_receiver::<T>,
        d: type_drop_receiver::<T>,
    };

    (tx, rx)
}

pub fn frame_channel<T>(bound: usize) -> (Sender<Vec<u8>>, Receiver<Vec<u8>>) {
    channel::<Vec<u8>>(bound)
}

fn deser_ref_sender<T: DeserializeOwned>(tx: *const mpsc::SyncSender<Never>, sli: &[u8]) -> SendRefOutcome {
    let tx: &mpsc::SyncSender<T> = unsafe { &*tx.cast() };

    let deser = match postcard::from_bytes(sli) {
        Ok(des) => des,
        Err(_) => {
            return SendRefOutcome::Failure;
        },
    };

    match tx.send(deser) {
        Ok(_) => SendRefOutcome::Success,
        Err(_) => SendRefOutcome::Failure,
    }
}

fn deser_sender<T: DeserializeOwned>(tx: *const mpsc::SyncSender<Never>, vec_in: *const Never) -> SendOutcome {
    let vec_in: *const Vec<u8> = vec_in.cast();
    let vec_in: Vec<u8> = unsafe { vec_in.read() };
    let tx: &mpsc::SyncSender<T> = unsafe { &*tx.cast() };

    let deser = match postcard::from_bytes(vec_in.as_slice()) {
        Ok(des) => des,
        Err(_) => {
            core::mem::forget(vec_in);
            return SendOutcome::Untaken;
        },
    };

    match tx.send(deser) {
        Ok(_) => SendOutcome::Taken,
        Err(_unsent_ty) => {
            core::mem::forget(vec_in);
            SendOutcome::Untaken
        },
    }
}

fn bypass_sender<T>(tx: *const mpsc::SyncSender<Never>, t: *const Never) -> SendOutcome {
    let t: *const T = t.cast();
    let t: T = unsafe { t.read() };
    let tx: &mpsc::SyncSender<T> = unsafe { &*tx.cast() };

    match tx.send(t) {
        Ok(()) => SendOutcome::Taken,
        Err(mpsc::SendError(t)) => {
            core::mem::forget(t);
            SendOutcome::Untaken
        }
    }
}

fn ser_receiver<T: Serialize>(
    rx: *const mpsc::Receiver<Never>,
    vec_out: *mut Never,
) -> RecvOutcome {
    let rx: &mpsc::Receiver<T> = unsafe { &*rx.cast() };
    let ty = match rx.recv() {
        Ok(t) => t,
        Err(_) => return RecvOutcome::Ungiven,
    };

    match postcard::to_stdvec(&ty) {
        Ok(v_out) => unsafe {
            vec_out.cast::<Vec<u8>>().write(v_out);
            RecvOutcome::Given
        },
        Err(_) => RecvOutcome::Ungiven,
    }
}

fn ser_ref_receiver<T: Serialize>(
    rx: *const mpsc::Receiver<Never>,
    sli: &mut [u8],
) -> Result<&mut [u8], ()> {
    let rx: &mpsc::Receiver<T> = unsafe { &*rx.cast() };
    let ty = match rx.recv() {
        Ok(t) => t,
        Err(_) => return Err(())
    };

    match postcard::to_slice(&ty, sli) {
        Ok(sli_out) => Ok(sli_out),
        Err(_) => Err(()),
    }
}

fn bypass_receiver<T>(rx: *const mpsc::Receiver<Never>, t: *mut Never) -> RecvOutcome {
    let rx: &mpsc::Receiver<T> = unsafe { &*rx.cast() };
    match rx.recv() {
        Ok(rec) => unsafe {
            t.cast::<T>().write(rec);
            RecvOutcome::Given
        },
        Err(_) => RecvOutcome::Ungiven,
    }
}

unsafe fn type_drop_sender<T>(container: &mut ManuallyDrop<mpsc::SyncSender<Never>>) {
    let container: &mut ManuallyDrop<mpsc::SyncSender<T>> = core::mem::transmute(container);
    ManuallyDrop::drop(container);
}

unsafe fn type_drop_receiver<T>(container: &mut ManuallyDrop<mpsc::Receiver<Never>>) {
    let container: &mut ManuallyDrop<mpsc::Receiver<T>> = core::mem::transmute(container);
    ManuallyDrop::drop(container);
}

// ---

use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use maitake_sync::{WaitCell, WaitQueue};

pub unsafe trait Storage<T> {
    fn buf(&self) -> (*const UnsafeCell<SpiteCell<T>>, usize);
}

pub struct MpScQueue<T, STO: Storage<T>> {
    storage: STO,
    dequeue_pos: AtomicUsize,
    enqueue_pos: AtomicUsize,
    cons_wait: WaitCell,
    prod_wait: WaitQueue,
    closed: AtomicBool,
    pd: PhantomData<T>,
}

/// Represents a closed error
#[derive(Debug, Eq, PartialEq)]
pub enum EnqueueError<T> {
    Full(T),
    Closed(T),
}

#[derive(Debug, Eq, PartialEq)]
pub enum DequeueError {
    Closed,
}

impl<T, STO: Storage<T>> MpScQueue<T, STO> {
    /// Creates an empty queue
    ///
    /// The capacity of `storage` must be >= 2 and a power of two, or this code will panic.
    #[track_caller]
    pub fn new(storage: STO) -> Self {
        let (ptr, len) = storage.buf();
        assert_eq!(
            len,
            len.next_power_of_two(),
            "Capacity must be a power of two!"
        );
        assert!(len > 1, "Capacity must be larger than 1!");
        let sli = unsafe { core::slice::from_raw_parts(ptr, len) };
        sli.iter().enumerate().for_each(|(i, slot)| unsafe {
            slot.get().write(SpiteCell {
                data: MaybeUninit::uninit(),
                sequence: AtomicUsize::new(i),
            });
        });

        Self {
            storage,
            dequeue_pos: AtomicUsize::new(0),
            enqueue_pos: AtomicUsize::new(0),
            cons_wait: WaitCell::new(),
            prod_wait: WaitQueue::new(),
            closed: AtomicBool::new(false),
            pd: PhantomData,
        }
    }

    // Mark the channel as permanently closed. Any already sent data
    // can be retrieved, but no further data will be allowed to be pushed.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.cons_wait.close();
        self.prod_wait.close();
    }

    /// Returns the item in the front of the queue, or `None` if the queue is empty
    pub fn dequeue_sync(&self) -> Option<T> {
        // Note: DON'T check the closed flag on dequeue. We want to be able
        // to drain any potential messages after closing.
        let (ptr, len) = self.storage.buf();
        let res = unsafe { dequeue((*ptr).get(), &self.dequeue_pos, len - 1) };
        if res.is_some() {
            self.prod_wait.wake_all();
        }
        res
    }

    /// Adds an `item` to the end of the queue
    ///
    /// Returns back the `item` if the queue is full
    pub fn enqueue_sync(&self, item: T) -> Result<(), EnqueueError<T>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(EnqueueError::Closed(item));
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
                err @ Err(EnqueueError::Closed(_)) => return err,

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

    pub async fn dequeue_async(&self) -> Result<T, DequeueError> {
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
}

unsafe impl<T, STO: Storage<T>> Sync for MpScQueue<T, STO> where T: Send {}

impl<T, STO: Storage<T>> Drop for MpScQueue<T, STO> {
    fn drop(&mut self) {
        while self.dequeue_sync().is_some() {}
        self.cons_wait.close();
        self.prod_wait.close();
    }
}

pub struct SpiteCell<T> {
    data: MaybeUninit<T>,
    sequence: AtomicUsize,
}

pub const fn single_cell<T>() -> SpiteCell<T> {
    SpiteCell {
        data: MaybeUninit::uninit(),
        sequence: AtomicUsize::new(0),
    }
}

pub fn cell_array<const N: usize, T: Sized>() -> [SpiteCell<T>; N] {
    [SpiteCell::<T>::SINGLE_CELL; N]
}

impl<T> SpiteCell<T> {
    const SINGLE_CELL: Self = Self::new(0);

    const fn new(seq: usize) -> Self {
        Self {
            data: MaybeUninit::uninit(),
            sequence: AtomicUsize::new(seq),
        }
    }
}

unsafe fn dequeue<T>(buffer: *mut SpiteCell<T>, dequeue_pos: &AtomicUsize, mask: usize) -> Option<T> {
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
    buffer: *mut SpiteCell<T>,
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
        let (tx, rx) = channel::<UnSerStruct>(4);
        tx.send(UnSerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        })
        .unwrap();

        tx.send(UnSerStruct {
            a: 20,
            b: -8000,
            c: 200_000,
            d: String::from("greets"),
        })
        .unwrap();

        tx.send(UnSerStruct {
            a: 100,
            b: -1_000,
            c: 300_000,
            d: String::from("oh my"),
        })
        .unwrap();

        assert_eq!(
            rx.recv().unwrap(),
            UnSerStruct {
                a: 240,
                b: -6_000,
                c: 100_000,
                d: String::from("hello"),
            }
        );

        assert_eq!(
            rx.recv().unwrap(),
            UnSerStruct {
                a: 20,
                b: -8000,
                c: 200_000,
                d: String::from("greets"),
            }
        );

        assert_eq!(
            rx.recv().unwrap(),
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
        let (tx, rx) = channel::<UnSerStruct>(4);
        drop(rx);
        let res = tx.send(UnSerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        });
        assert_eq!(res, Err(UnSerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        }));
    }

    #[test]
    fn normal_closed_tx() {
        let (tx, rx) = channel::<UnSerStruct>(4);
        drop(tx);
        assert_eq!(rx.recv(), Err(RecvError::Oops));
    }

    #[test]
    fn ser_smoke() {
        let (tx, rx) = ser_channel::<SerStruct>(4);
        tx.send(SerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        })
        .unwrap();

        tx.send(SerStruct {
            a: 20,
            b: -8000,
            c: 200_000,
            d: String::from("greets"),
        })
        .unwrap();

        tx.send(SerStruct {
            a: 100,
            b: -1_000,
            c: 300_000,
            d: String::from("oh my"),
        })
        .unwrap();

        assert_eq!(
            rx.recv().unwrap(),
            vec![240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]
        );

        assert_eq!(
            rx.recv().unwrap(),
            vec![20, 255, 124, 192, 154, 12, 6, 103, 114, 101, 101, 116, 115]
        );

        assert_eq!(
            rx.recv().unwrap(),
            vec![100, 207, 15, 224, 167, 18, 5, 111, 104, 32, 109, 121]
        );
    }

    #[test]
    fn ser_closed_rx() {
        let (tx, rx) = ser_channel::<SerStruct>(4);
        drop(rx);
        let res = tx.send(SerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        });
        assert_eq!(res, Err(SerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        }));
    }

    #[test]
    fn ser_closed_tx() {
        let (tx, rx) = ser_channel::<SerStruct>(4);
        drop(tx);
        assert_eq!(rx.recv(), Err(RecvError::Oops));
    }


    #[test]
    fn ser_ref_smoke() {
        let (tx, rx) = ser_ref_channel::<SerStruct>(4);
        tx.send(SerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        })
        .unwrap();

        tx.send(SerStruct {
            a: 20,
            b: -8000,
            c: 200_000,
            d: String::from("greets"),
        })
        .unwrap();

        tx.send(SerStruct {
            a: 100,
            b: -1_000,
            c: 300_000,
            d: String::from("oh my"),
        })
        .unwrap();

        let mut buf = [0u8; 128];

        assert_eq!(
            rx.recv_slice(&mut buf).unwrap(),
            &mut [240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]
        );

        assert_eq!(
            rx.recv_slice(&mut buf).unwrap(),
            &mut [20, 255, 124, 192, 154, 12, 6, 103, 114, 101, 101, 116, 115]
        );

        assert_eq!(
            rx.recv_slice(&mut buf).unwrap(),
            &mut [100, 207, 15, 224, 167, 18, 5, 111, 104, 32, 109, 121]
        );
    }

    #[test]
    fn ser_ref_closed_rx() {
        let (tx, rx) = ser_ref_channel::<SerStruct>(4);
        drop(rx);
        let res = tx.send(SerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        });
        assert_eq!(res, Err(SerStruct {
            a: 240,
            b: -6_000,
            c: 100_000,
            d: String::from("hello"),
        }));
    }

    #[test]
    fn ser_ref_closed_tx() {
        let (tx, rx) = ser_ref_channel::<SerStruct>(4);
        drop(tx);
        let mut buf = [0u8; 128];
        assert_eq!(rx.recv_slice(&mut buf), Err(()));
    }

    #[test]
    fn deser_smoke() {
        let (tx, rx) = deser_channel::<DeStruct>(4);
        tx.send(vec![240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111])
            .unwrap();

        tx.send(vec![
            20, 255, 124, 192, 154, 12, 6, 103, 114, 101, 101, 116, 115,
        ])
        .unwrap();

        tx.send(vec![100, 207, 15, 224, 167, 18, 5, 111, 104, 32, 109, 121])
            .unwrap();

        assert_eq!(
            rx.recv().unwrap(),
            DeStruct {
                a: 240,
                b: -6_000,
                c: 100_000,
                d: String::from("hello"),
            }
        );

        assert_eq!(
            rx.recv().unwrap(),
            DeStruct {
                a: 20,
                b: -8000,
                c: 200_000,
                d: String::from("greets"),
            }
        );

        assert_eq!(
            rx.recv().unwrap(),
            DeStruct {
                a: 100,
                b: -1_000,
                c: 300_000,
                d: String::from("oh my"),
            }
        );
    }

    #[test]
    fn deser_closed_rx() {
        let (tx, rx) = deser_channel::<DeStruct>(4);
        drop(rx);
        let res = tx.send(vec![240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]);
        assert_eq!(res, Err(vec![240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]));
    }

    #[test]
    fn deser_closed_tx() {
        let (tx, rx) = deser_channel::<DeStruct>(4);
        drop(tx);
        assert_eq!(rx.recv(), Err(RecvError::Oops));
    }


    #[test]
    fn deser_ref_smoke() {
        let (tx, rx) = deser_ref_channel::<DeStruct>(4);
        tx.send_slice(&[240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111])
            .unwrap();

        tx.send_slice(&[
            20, 255, 124, 192, 154, 12, 6, 103, 114, 101, 101, 116, 115,
        ])
        .unwrap();

        tx.send_slice(&[100, 207, 15, 224, 167, 18, 5, 111, 104, 32, 109, 121])
            .unwrap();

        assert_eq!(
            rx.recv().unwrap(),
            DeStruct {
                a: 240,
                b: -6_000,
                c: 100_000,
                d: String::from("hello"),
            }
        );

        assert_eq!(
            rx.recv().unwrap(),
            DeStruct {
                a: 20,
                b: -8000,
                c: 200_000,
                d: String::from("greets"),
            }
        );

        assert_eq!(
            rx.recv().unwrap(),
            DeStruct {
                a: 100,
                b: -1_000,
                c: 300_000,
                d: String::from("oh my"),
            }
        );
    }

    #[test]
    fn deser_ref_closed_rx() {
        let (tx, rx) = deser_ref_channel::<DeStruct>(4);
        drop(rx);
        let res = tx.send_slice(&[240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]);
        assert_eq!(res, Err(()));
    }

    #[test]
    fn deser_ref_closed_tx() {
        let (tx, rx) = deser_ref_channel::<DeStruct>(4);
        drop(tx);
        assert_eq!(rx.recv(), Err(RecvError::Oops));
    }
}
