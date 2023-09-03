use std::marker::PhantomData;
use std::mem::{MaybeUninit, ManuallyDrop, transmute};
use std::ops::Deref;
use std::sync::mpsc;

use serde::de::DeserializeOwned;
use serde::Serialize;

enum Never {}

enum SerOutcome {
    Taken,
    Untaken,
}

enum DeserOutcome {
    Given,
    Ungiven,
}

type SendFunc = fn(*const mpsc::SyncSender<Never>, *const Never) -> SerOutcome;
type RecvFunc = fn(*const mpsc::Receiver<Never>, *mut Never) -> DeserOutcome;
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
            SerOutcome::Taken => Ok(()),
            SerOutcome::Untaken => Err(unsafe { inbox.assume_init() }),
        }
    }
}

impl<T> Drop for Sender<T> {
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

pub enum RecvError {
    Oops
}

impl<T> Receiver<T> {
    pub fn recv(&self) -> Result<T, RecvError> {
        let mut outbox: MaybeUninit<T> = MaybeUninit::uninit();
        let outref: *mut T = outbox.as_mut_ptr();
        let outref: *mut Never = outref.cast();

        match (self.f)(self.rx.deref(), outref) {
            DeserOutcome::Given => {
                Ok(unsafe { outbox.assume_init() })
            },
            DeserOutcome::Ungiven => Err(RecvError::Oops),
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        unsafe { (self.d)(&mut self.rx) }
    }
}

pub fn channel<T>(bound: usize) -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::sync_channel::<T>(bound);
    let tx: ManuallyDrop<mpsc::SyncSender<Never>> = unsafe {
        transmute(ManuallyDrop::new(tx))
    };
    let rx: ManuallyDrop<mpsc::Receiver<Never>> = unsafe {
        transmute(ManuallyDrop::new(rx))
    };

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
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(bound);
    let tx: ManuallyDrop<mpsc::SyncSender<Never>> = unsafe {
        transmute(ManuallyDrop::new(tx))
    };
    let rx: ManuallyDrop<mpsc::Receiver<Never>> = unsafe {
        transmute(ManuallyDrop::new(rx))
    };

    let tx: Sender<T> = Sender {
        _pd: PhantomData,
        tx,
        f: ser_sender::<T>,
        d: type_drop_sender::<T>,
    };

    let rx: Receiver<Vec<u8>> = Receiver {
        _pd: PhantomData,
        rx,
        f: bypass_receiver::<T>,
        d: type_drop_receiver::<T>,
    };

    (tx, rx)
}

pub fn deser_channel<T: DeserializeOwned>(bound: usize) -> (Sender<Vec<u8>>, Receiver<T>) {
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(bound);
    let tx: ManuallyDrop<mpsc::SyncSender<Never>> = unsafe {
        transmute(ManuallyDrop::new(tx))
    };
    let rx: ManuallyDrop<mpsc::Receiver<Never>> = unsafe {
        transmute(ManuallyDrop::new(rx))
    };

    let tx: Sender<Vec<u8>> = Sender {
        _pd: PhantomData,
        tx,
        f: bypass_sender::<T>,
        d: type_drop_sender::<T>,
    };

    let rx: Receiver<T> = Receiver {
        _pd: PhantomData,
        rx,
        f: deser_receiver::<T>,
        d: type_drop_receiver::<T>,
    };

    (tx, rx)
}

pub fn frame_channel<T>(bound: usize) -> (Sender<Vec<u8>>, Receiver<Vec<u8>>) {
    channel::<Vec<u8>>(bound)
}

fn ser_sender<T: Serialize>(tx: *const mpsc::SyncSender<Never>, t: *const Never) -> SerOutcome {
    let t: *const T = t.cast();
    let t: T = unsafe { t.read() };
    let tx: &mpsc::SyncSender<Vec<u8>> = unsafe { &*tx.cast() };

    let ser = match postcard::to_stdvec(&t) {
        Ok(s) => s,
        Err(_) => {
            core::mem::forget(t);
            return SerOutcome::Untaken;
        }
    };

    match tx.send(ser) {
        Ok(()) => SerOutcome::Taken,
        Err(mpsc::SendError(t)) => {
            core::mem::forget(t);
            SerOutcome::Untaken
        }
    }
}

fn bypass_sender<T>(tx: *const mpsc::SyncSender<Never>, t: *const Never) -> SerOutcome {
    let t: *const T = t.cast();
    let t: T = unsafe { t.read() };
    let tx: &mpsc::SyncSender<T> = unsafe { &*tx.cast() };

    match tx.send(t) {
        Ok(()) => SerOutcome::Taken,
        Err(mpsc::SendError(t)) => {
            core::mem::forget(t);
            SerOutcome::Untaken
        }
    }
}

fn deser_receiver<T: DeserializeOwned>(
    rx: *const mpsc::Receiver<Never>,
    t: *mut Never,
) -> DeserOutcome {
    let rx: &mpsc::Receiver<Vec<u8>> = unsafe { &*rx.cast() };
    let buf = match rx.recv() {
        Ok(v) => v,
        Err(_) => {
            return DeserOutcome::Ungiven;
        }
    };

    match postcard::from_bytes::<T>(&buf) {
        Ok(deser) => unsafe {
            t.cast::<T>().write(deser);
            DeserOutcome::Given
        },
        Err(_) => DeserOutcome::Ungiven,
    }
}

fn bypass_receiver<T>(
    rx: *const mpsc::Receiver<Never>,
    t: *mut Never,
) -> DeserOutcome {
    let rx: &mpsc::Receiver<T> = unsafe { &*rx.cast() };
    match rx.recv() {
        Ok(rec) => unsafe {
            t.cast::<T>().write(rec);
            DeserOutcome::Given
        },
        Err(_) => {
            DeserOutcome::Ungiven
        }
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
