use std::marker::PhantomData;
use std::mem::{transmute, ManuallyDrop, MaybeUninit};
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
            DeserOutcome::Given => Ok(unsafe { outbox.assume_init() }),
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
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(bound);
    let tx: ManuallyDrop<mpsc::SyncSender<Never>> = unsafe { transmute(ManuallyDrop::new(tx)) };
    let rx: ManuallyDrop<mpsc::Receiver<Never>> = unsafe { transmute(ManuallyDrop::new(rx)) };

    let tx: Sender<T> = Sender {
        _pd: PhantomData,
        tx,
        f: ser_sender::<T>,
        d: type_drop_sender::<Vec<u8>>,
    };

    let rx: Receiver<Vec<u8>> = Receiver {
        _pd: PhantomData,
        rx,
        f: bypass_receiver::<Vec<u8>>,
        d: type_drop_receiver::<Vec<u8>>,
    };

    (tx, rx)
}

pub fn deser_channel<T: DeserializeOwned>(bound: usize) -> (Sender<Vec<u8>>, Receiver<T>) {
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(bound);
    let tx: ManuallyDrop<mpsc::SyncSender<Never>> = unsafe { transmute(ManuallyDrop::new(tx)) };
    let rx: ManuallyDrop<mpsc::Receiver<Never>> = unsafe { transmute(ManuallyDrop::new(rx)) };

    let tx: Sender<Vec<u8>> = Sender {
        _pd: PhantomData,
        tx,
        f: bypass_sender::<Vec<u8>>,
        d: type_drop_sender::<Vec<u8>>,
    };

    let rx: Receiver<T> = Receiver {
        _pd: PhantomData,
        rx,
        f: deser_receiver::<T>,
        d: type_drop_receiver::<Vec<u8>>,
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
        Err(mpsc::SendError(_unsent_vec)) => {
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

fn bypass_receiver<T>(rx: *const mpsc::Receiver<Never>, t: *mut Never) -> DeserOutcome {
    let rx: &mpsc::Receiver<T> = unsafe { &*rx.cast() };
    match rx.recv() {
        Ok(rec) => unsafe {
            t.cast::<T>().write(rec);
            DeserOutcome::Given
        },
        Err(_) => DeserOutcome::Ungiven,
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
}
