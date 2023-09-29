use core::cell::UnsafeCell;
use core::debug_assert;
use core::marker::PhantomData;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ops::Deref;

use serde::de::DeserializeOwned;
use serde::Serialize;
use spitebuf::EnqueueError;

use crate::spitebuf;

enum Never {}

unsafe impl spitebuf::Storage<Never> for Never {
    fn buf(&self) -> (*const UnsafeCell<spitebuf::Cell<Never>>, usize) {
        debug_assert!(false);
        (core::ptr::null(), 0)
    }
}

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

type SendFunc = fn(*const spitebuf::Sender<Never, Never>, *const Never) -> SendOutcome;
type SendRefFunc = fn(*const spitebuf::Sender<Never, Never>, &[u8]) -> SendRefOutcome;
type RecvFunc = fn(*const spitebuf::Receiver<Never, Never>, *mut Never) -> RecvOutcome;
type RecvRefFunc = fn(*const spitebuf::Receiver<Never, Never>, &mut [u8]) -> Result<&mut [u8], ()>;
type SenderDropFunc = unsafe fn(&mut ManuallyDrop<spitebuf::Sender<Never, Never>>);
type ReceiverDropFunc = unsafe fn(&mut ManuallyDrop<spitebuf::Receiver<Never, Never>>);

// TODO: does this need STO? or can that be monomorphized?
pub struct Sender<T> {
    _pd: PhantomData<T>,
    tx: ManuallyDrop<spitebuf::Sender<Never, Never>>,
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
    tx: ManuallyDrop<spitebuf::Sender<Never, Never>>,
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
    rx: ManuallyDrop<spitebuf::Receiver<Never, Never>>,
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
    rx: ManuallyDrop<spitebuf::Receiver<Never, Never>>,
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

#[cfg(TODO)]
mod alloc {
    pub fn channel<T>(bound: usize) -> (Sender<T>, Receiver<T>) {
        let (tx, rx) = mpsc::sync_channel::<T>(bound);
        let tx: ManuallyDrop<spitebuf::Sender<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(tx)) };
        let rx: ManuallyDrop<spitebuf::Receiver<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(rx)) };

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
        let tx: ManuallyDrop<spitebuf::Sender<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(tx)) };
        let rx: ManuallyDrop<spitebuf::Receiver<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(rx)) };

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
        let tx: ManuallyDrop<spitebuf::Sender<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(tx)) };
        let rx: ManuallyDrop<spitebuf::Receiver<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(rx)) };

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

    pub fn deser_channel<T: DeserializeOwned + 'static, STO: spitebuf::Storage<T> + 'static>(
        bound: usize,
    ) -> (Sender<Vec<u8>>, Receiver<T>) {
        let (tx, rx) = mpsc::sync_channel::<T>(bound);
        let tx: ManuallyDrop<spitebuf::Sender<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(tx)) };
        let rx: ManuallyDrop<spitebuf::Receiver<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(rx)) };

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

    pub fn deser_ref_channel<T: DeserializeOwned + 'static, STO: spitebuf::Storage<T> + 'static>(
        bound: usize,
    ) -> (RefSender, Receiver<T>) {
        let (tx, rx) = mpsc::sync_channel::<T>(bound);
        let tx: ManuallyDrop<spitebuf::Sender<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(tx)) };
        let rx: ManuallyDrop<spitebuf::Receiver<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(rx)) };

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
}

fn deser_ref_sender<T: DeserializeOwned + 'static, STO: spitebuf::Storage<T> + 'static>(
    tx: *const spitebuf::Sender<Never, Never>,
    sli: &[u8],
) -> SendRefOutcome {
    let tx: &spitebuf::Sender<T, STO> = unsafe { &*tx.cast() };

    let deser = match postcard::from_bytes(sli) {
        Ok(des) => des,
        Err(_) => {
            return SendRefOutcome::Failure;
        }
    };

    match tx.enqueue_sync(deser) {
        Ok(_) => SendRefOutcome::Success,
        Err(_) => SendRefOutcome::Failure,
    }
}

#[cfg(TODO)]
fn deser_sender<T: DeserializeOwned + 'static, STO: spitebuf::Storage<T> + 'static>(
    tx: *const spitebuf::Sender<Never, Never>,
    vec_in: *const Never,
) -> SendOutcome {
    let vec_in: *const Vec<u8> = vec_in.cast();
    let vec_in: Vec<u8> = unsafe { vec_in.read() };
    let tx: &spitebuf::Sender<T, STO> = unsafe { &*tx.cast() };

    let deser = match postcard::from_bytes(vec_in.as_slice()) {
        Ok(des) => des,
        Err(_) => {
            core::mem::forget(vec_in);
            return SendOutcome::Untaken;
        }
    };

    match tx.enqueue_sync(deser) {
        Ok(_) => SendOutcome::Taken,
        Err(_unsent_ty) => {
            core::mem::forget(vec_in);
            SendOutcome::Untaken
        }
    }
}

pub mod stat {
    use core::{mem::{ManuallyDrop, transmute}, marker::PhantomData};

    use serde::{Serialize, de::DeserializeOwned};

    use crate::tp1::{spitebuf::{Storage, MpScQueue, self}, Never, Sender, Receiver, bypass_sender, type_drop_sender, bypass_receiver, type_drop_receiver, RefReceiver, ser_ref_receiver, RefSender, deser_ref_sender};

    pub fn channel<T: 'static, STO: Storage<T> + 'static>(q: &'static MpScQueue<T, STO>) -> (Sender<T>, Receiver<T>) {
        let (tx, rx) = q.init_split().unwrap();
        let tx: ManuallyDrop<crate::spitebuf::Sender<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(tx)) };
        let rx: ManuallyDrop<crate::spitebuf::Receiver<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(rx)) };

        let tx: Sender<T> = Sender {
            _pd: PhantomData,
            tx,
            f: bypass_sender::<T, STO>,
            d: type_drop_sender::<T, STO>,
        };

        let rx: Receiver<T> = Receiver {
            _pd: PhantomData,
            rx,
            f: bypass_receiver::<T, STO>,
            d: type_drop_receiver::<T, STO>,
        };

        (tx, rx)
    }

    pub fn ser_ref_channel<T: Serialize + 'static, STO: spitebuf::Storage<T> + 'static>(q: &'static MpScQueue<T, STO>) -> (Sender<T>, RefReceiver) {
        let (tx, rx) = q.init_split().unwrap();
        let tx: ManuallyDrop<spitebuf::Sender<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(tx)) };
        let rx: ManuallyDrop<spitebuf::Receiver<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(rx)) };

        let tx: Sender<T> = Sender {
            _pd: PhantomData,
            tx,
            f: bypass_sender::<T, STO>,
            d: type_drop_sender::<T, STO>,
        };

        let rx: RefReceiver = RefReceiver {
            rx,
            f: ser_ref_receiver::<T, STO>,
            d: type_drop_receiver::<T, STO>,
        };

        (tx, rx)
    }

    pub fn deser_ref_channel<T: DeserializeOwned + 'static, STO: spitebuf::Storage<T> + 'static>(
        q: &'static MpScQueue<T, STO>
    ) -> (RefSender, Receiver<T>) {
        let (tx, rx) = q.init_split().unwrap();
        let tx: ManuallyDrop<spitebuf::Sender<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(tx)) };
        let rx: ManuallyDrop<spitebuf::Receiver<Never, Never>> =
            unsafe { transmute(ManuallyDrop::new(rx)) };

        let tx: RefSender = RefSender {
            tx,
            f: deser_ref_sender::<T, STO>,
            d: type_drop_sender::<T, STO>,
        };

        let rx: Receiver<T> = Receiver {
            _pd: PhantomData,
            rx,
            f: bypass_receiver::<T, STO>,
            d: type_drop_receiver::<T, STO>,
        };

        (tx, rx)
    }

    // pub fn frame_channel<T>(bound: usize) -> (Sender<Vec<u8>>, Receiver<Vec<u8>>) {
    //     channel::<Vec<u8>>(bound)
    // }
}

#[cfg(TODO)]
fn deser_sender<T: DeserializeOwned + 'static, STO: spitebuf::Storage<T> + 'static>(
    tx: *const spitebuf::Sender<Never, Never>,
    vec_in: *const Never,
) -> SendOutcome {
    let vec_in: *const Vec<u8> = vec_in.cast();
    let vec_in: Vec<u8> = unsafe { vec_in.read() };
    let tx: &spitebuf::Sender<T, STO> = unsafe { &*tx.cast() };

    let deser = match postcard::from_bytes(vec_in.as_slice()) {
        Ok(des) => des,
        Err(_) => {
            core::mem::forget(vec_in);
            return SendOutcome::Untaken;
        }
    };

    match tx.enqueue_sync(deser) {
        Ok(_) => SendOutcome::Taken,
        Err(_unsent_ty) => {
            core::mem::forget(vec_in);
            SendOutcome::Untaken
        }
    }
}

fn bypass_sender<T: 'static, STO: spitebuf::Storage<T> + 'static>(
    tx: *const spitebuf::Sender<Never, Never>,
    t: *const Never,
) -> SendOutcome {
    let t: *const T = t.cast();
    let t: T = unsafe { t.read() };
    let tx: &spitebuf::Sender<T, STO> = unsafe { &*tx.cast() };

    match tx.enqueue_sync(t) {
        Ok(()) => SendOutcome::Taken,
        Err(EnqueueError::Full(t))
        | Err(EnqueueError::Closed(t))
        | Err(EnqueueError::InternalError(t)) => {
            core::mem::forget(t);
            SendOutcome::Untaken
        }
    }
}

#[cfg(TODO)]
fn ser_receiver<T: Serialize + 'static, STO: spitebuf::Storage<T> + 'static>(
    rx: *const spitebuf::Receiver<Never, Never>,
    vec_out: *mut Never,
) -> RecvOutcome {
    let rx: &spitebuf::Receiver<T, STO> = unsafe { &*rx.cast() };
    let ty = match rx.dequeue_sync() {
        Some(t) => t,
        None => return RecvOutcome::Ungiven,
    };

    match postcard::to_stdvec(&ty) {
        Ok(v_out) => unsafe {
            vec_out.cast::<Vec<u8>>().write(v_out);
            RecvOutcome::Given
        },
        Err(_) => RecvOutcome::Ungiven,
    }
}

fn ser_ref_receiver<T: Serialize + 'static, STO: spitebuf::Storage<T> + 'static>(
    rx: *const spitebuf::Receiver<Never, Never>,
    sli: &mut [u8],
) -> Result<&mut [u8], ()> {
    let rx: &spitebuf::Receiver<T, STO> = unsafe { &*rx.cast() };
    let ty = match rx.dequeue_sync() {
        Some(t) => t,
        None => return Err(()),
    };

    match postcard::to_slice(&ty, sli) {
        Ok(sli_out) => Ok(sli_out),
        Err(_) => Err(()),
    }
}

fn bypass_receiver<T: 'static, STO: spitebuf::Storage<T> + 'static>(
    rx: *const spitebuf::Receiver<Never, Never>,
    t: *mut Never,
) -> RecvOutcome {
    let rx: &spitebuf::Receiver<T, STO> = unsafe { &*rx.cast() };
    match rx.dequeue_sync() {
        Some(rec) => unsafe {
            t.cast::<T>().write(rec);
            RecvOutcome::Given
        },
        None => RecvOutcome::Ungiven,
    }
}

unsafe fn type_drop_sender<T: 'static, STO: spitebuf::Storage<T> + 'static>(
    container: &mut ManuallyDrop<spitebuf::Sender<Never, Never>>,
) {
    let container: &mut ManuallyDrop<spitebuf::Sender<T, STO>> = core::mem::transmute(container);
    ManuallyDrop::drop(container);
}

unsafe fn type_drop_receiver<T: 'static, STO: spitebuf::Storage<T> + 'static>(
    container: &mut ManuallyDrop<spitebuf::Receiver<Never, Never>>,
) {
    let container: &mut ManuallyDrop<spitebuf::Receiver<T, STO>> = core::mem::transmute(container);
    ManuallyDrop::drop(container);
}

#[cfg(test)]
mod test {
    use crate::spitebuf::Storage;

    use super::*;
    use serde::Deserialize;

    struct Vecky<T> {
        v: Vec<UnsafeCell<crate::spitebuf::Cell<T>>>,
    }

    unsafe impl<T> Storage<T> for Vecky<T> {
        fn buf(&self) -> (*const UnsafeCell<spitebuf::Cell<T>>, usize) {
            let len = self.v.len();
            (self.v.as_ptr(), len)
        }
    }

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

    // #[test]
    // fn normal_smoke() {
    //     let (tx, rx) = channel::<UnSerStruct>(4);
    //     tx.send(UnSerStruct {
    //         a: 240,
    //         b: -6_000,
    //         c: 100_000,
    //         d: String::from("hello"),
    //     })
    //     .unwrap();

    //     tx.send(UnSerStruct {
    //         a: 20,
    //         b: -8000,
    //         c: 200_000,
    //         d: String::from("greets"),
    //     })
    //     .unwrap();

    //     tx.send(UnSerStruct {
    //         a: 100,
    //         b: -1_000,
    //         c: 300_000,
    //         d: String::from("oh my"),
    //     })
    //     .unwrap();

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         UnSerStruct {
    //             a: 240,
    //             b: -6_000,
    //             c: 100_000,
    //             d: String::from("hello"),
    //         }
    //     );

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         UnSerStruct {
    //             a: 20,
    //             b: -8000,
    //             c: 200_000,
    //             d: String::from("greets"),
    //         }
    //     );

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         UnSerStruct {
    //             a: 100,
    //             b: -1_000,
    //             c: 300_000,
    //             d: String::from("oh my"),
    //         }
    //     );
    // }

    // #[test]
    // fn normal_closed_rx() {
    //     let (tx, rx) = channel::<UnSerStruct>(4);
    //     drop(rx);
    //     let res = tx.send(UnSerStruct {
    //         a: 240,
    //         b: -6_000,
    //         c: 100_000,
    //         d: String::from("hello"),
    //     });
    //     assert_eq!(
    //         res,
    //         Err(UnSerStruct {
    //             a: 240,
    //             b: -6_000,
    //             c: 100_000,
    //             d: String::from("hello"),
    //         })
    //     );
    // }

    // #[test]
    // fn normal_closed_tx() {
    //     let (tx, rx) = channel::<UnSerStruct>(4);
    //     drop(tx);
    //     assert_eq!(rx.recv(), Err(RecvError::Oops));
    // }

    // #[test]
    // fn ser_smoke() {
    //     let (tx, rx) = ser_channel::<SerStruct>(4);
    //     tx.send(SerStruct {
    //         a: 240,
    //         b: -6_000,
    //         c: 100_000,
    //         d: String::from("hello"),
    //     })
    //     .unwrap();

    //     tx.send(SerStruct {
    //         a: 20,
    //         b: -8000,
    //         c: 200_000,
    //         d: String::from("greets"),
    //     })
    //     .unwrap();

    //     tx.send(SerStruct {
    //         a: 100,
    //         b: -1_000,
    //         c: 300_000,
    //         d: String::from("oh my"),
    //     })
    //     .unwrap();

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         vec![240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]
    //     );

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         vec![20, 255, 124, 192, 154, 12, 6, 103, 114, 101, 101, 116, 115]
    //     );

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         vec![100, 207, 15, 224, 167, 18, 5, 111, 104, 32, 109, 121]
    //     );
    // }

    // #[test]
    // fn ser_closed_rx() {
    //     let (tx, rx) = ser_channel::<SerStruct>(4);
    //     drop(rx);
    //     let res = tx.send(SerStruct {
    //         a: 240,
    //         b: -6_000,
    //         c: 100_000,
    //         d: String::from("hello"),
    //     });
    //     assert_eq!(
    //         res,
    //         Err(SerStruct {
    //             a: 240,
    //             b: -6_000,
    //             c: 100_000,
    //             d: String::from("hello"),
    //         })
    //     );
    // }

    // #[test]
    // fn ser_closed_tx() {
    //     let (tx, rx) = ser_channel::<SerStruct>(4);
    //     drop(tx);
    //     assert_eq!(rx.recv(), Err(RecvError::Oops));
    // }

    #[test]
    fn ser_ref_smoke() {
        let (tx, rx) = stat::ser_ref_channel::<SerStruct>(4);
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
        assert_eq!(
            res,
            Err(SerStruct {
                a: 240,
                b: -6_000,
                c: 100_000,
                d: String::from("hello"),
            })
        );
    }

    #[test]
    fn ser_ref_closed_tx() {
        let (tx, rx) = ser_ref_channel::<SerStruct>(4);
        drop(tx);
        let mut buf = [0u8; 128];
        assert_eq!(rx.recv_slice(&mut buf), Err(()));
    }

    // #[test]
    // fn deser_smoke() {
    //     let (tx, rx) = deser_channel::<DeStruct>(4);
    //     tx.send(vec![240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111])
    //         .unwrap();

    //     tx.send(vec![
    //         20, 255, 124, 192, 154, 12, 6, 103, 114, 101, 101, 116, 115,
    //     ])
    //     .unwrap();

    //     tx.send(vec![100, 207, 15, 224, 167, 18, 5, 111, 104, 32, 109, 121])
    //         .unwrap();

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         DeStruct {
    //             a: 240,
    //             b: -6_000,
    //             c: 100_000,
    //             d: String::from("hello"),
    //         }
    //     );

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         DeStruct {
    //             a: 20,
    //             b: -8000,
    //             c: 200_000,
    //             d: String::from("greets"),
    //         }
    //     );

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         DeStruct {
    //             a: 100,
    //             b: -1_000,
    //             c: 300_000,
    //             d: String::from("oh my"),
    //         }
    //     );
    // }

    // #[test]
    // fn deser_closed_rx() {
    //     let (tx, rx) = deser_channel::<DeStruct>(4);
    //     drop(rx);
    //     let res = tx.send(vec![240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]);
    //     assert_eq!(
    //         res,
    //         Err(vec![240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111])
    //     );
    // }

    // #[test]
    // fn deser_closed_tx() {
    //     let (tx, rx) = deser_channel::<DeStruct>(4);
    //     drop(tx);
    //     assert_eq!(rx.recv(), Err(RecvError::Oops));
    // }

    // #[test]
    // fn deser_ref_smoke() {
    //     let (tx, rx) = deser_ref_channel::<DeStruct>(4);
    //     tx.send_slice(&[240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111])
    //         .unwrap();

    //     tx.send_slice(&[20, 255, 124, 192, 154, 12, 6, 103, 114, 101, 101, 116, 115])
    //         .unwrap();

    //     tx.send_slice(&[100, 207, 15, 224, 167, 18, 5, 111, 104, 32, 109, 121])
    //         .unwrap();

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         DeStruct {
    //             a: 240,
    //             b: -6_000,
    //             c: 100_000,
    //             d: String::from("hello"),
    //         }
    //     );

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         DeStruct {
    //             a: 20,
    //             b: -8000,
    //             c: 200_000,
    //             d: String::from("greets"),
    //         }
    //     );

    //     assert_eq!(
    //         rx.recv().unwrap(),
    //         DeStruct {
    //             a: 100,
    //             b: -1_000,
    //             c: 300_000,
    //             d: String::from("oh my"),
    //         }
    //     );
    // }

    // #[test]
    // fn deser_ref_closed_rx() {
    //     let (tx, rx) = deser_ref_channel::<DeStruct>(4);
    //     drop(rx);
    //     let res = tx.send_slice(&[240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]);
    //     assert_eq!(res, Err(()));
    // }

    // #[test]
    // fn deser_ref_closed_tx() {
    //     let (tx, rx) = deser_ref_channel::<DeStruct>(4);
    //     drop(tx);
    //     assert_eq!(rx.recv(), Err(RecvError::Oops));
    // }
}
