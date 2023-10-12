use super::*;
use crate::loom::sync::Arc;
use alloc::boxed::Box;

use super::channel_core::{Core, CoreVtable};

pub struct TrickyPipe<T: 'static>(Arc<Inner<T>>);

struct Inner<T: 'static> {
    core: Core,
    elements: Box<[Cell<T>]>,
}

impl<T: 'static> TrickyPipe<T> {
    // TODO(eliza): we would need to add a mnemos-alloc version of this...
    pub fn new(capacity: u8) -> Self {
        Self(Arc::new(Inner {
            core: Core::new(capacity),
            elements: (0..capacity)
                .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
                .collect(),
        }))
    }

    const CORE_VTABLE: &'static CoreVtable = &CoreVtable {
        get_core: Self::get_core,
        get_elems: Self::get_elems,
        clone: Self::erased_clone,
        drop: Self::erased_drop,
    };

    fn erased(&self) -> ErasedPipe {
        let ptr = Arc::into_raw(self.0.clone()) as *const _;
        unsafe { ErasedPipe::new(ptr, Self::CORE_VTABLE) }
    }

    fn typed(&self) -> TypedPipe<T> {
        unsafe { self.erased().typed() }
    }

    pub fn receiver(&self) -> Option<Receiver<T>> {
        self.0.core.try_claim_rx()?;

        Some(Receiver { pipe: self.typed() })
    }

    pub fn sender(&self) -> Sender<T> {
        self.0.core.add_tx();
        Sender { pipe: self.typed() }
    }

    unsafe fn get_core(ptr: *const ()) -> *const Core {
        unsafe {
            let ptr = ptr.cast::<Inner<T>>();
            ptr::addr_of!((*ptr).core)
        }
    }

    unsafe fn get_elems(ptr: *const ()) -> ErasedSlice {
        let ptr = ptr.cast::<Inner<T>>();
        ErasedSlice::erase(&(*ptr).elements)
    }

    unsafe fn erased_clone(ptr: *const ()) {
        test_println!("erased_clone({ptr:p})");
        Arc::increment_strong_count(ptr.cast::<Inner<T>>())
    }

    unsafe fn erased_drop(ptr: *const ()) {
        let arc = Arc::from_raw(ptr.cast::<Inner<T>>());
        test_println!(refs = Arc::strong_count(&arc), "erased_drop({ptr:p})");
        drop(arc)
    }
}

impl<T: Serialize + 'static> TrickyPipe<T> {
    pub fn ser_receiver(&self) -> Option<SerReceiver> {
        self.0.core.try_claim_rx()?;

        Some(SerReceiver {
            pipe: self.erased(),
            vtable: Self::SER_VTABLE,
        })
    }

    const SER_VTABLE: &'static SerVtable = &SerVtable {
        #[cfg(any(test, feature = "alloc"))]
        to_vec: SerVtable::to_vec::<T>,
        #[cfg(any(test, feature = "alloc"))]
        to_vec_framed: SerVtable::to_vec_framed::<T>,
        to_slice: SerVtable::to_slice::<T>,
        to_slice_framed: SerVtable::to_slice_framed::<T>,
    };
}

impl<T: DeserializeOwned + 'static> TrickyPipe<T> {
    pub fn ser_sender(&self) -> SerSender {
        self.0.core.add_tx();
        SerSender {
            pipe: self.erased(),
            vtable: Self::DESER_VTABLE,
        }
    }

    const DESER_VTABLE: &'static DeserVtable = &DeserVtable::new::<T>();
}

unsafe impl<T: Send> Send for TrickyPipe<T> {}
unsafe impl<T: Send> Sync for TrickyPipe<T> {}

impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        test_span!("Inner::drop");

        // TODO(eliza): there is probably a more efficient way to implement this
        // rather than by using `try_dequeue`, since we know that we have
        // exclusive ownership over the queue. But this works.
        while let Ok(res) = self.core.try_dequeue() {
            let idx = res.idx as usize;
            self.elements[test_dbg!(idx)].with_mut(|ptr| unsafe {
                (*ptr).as_mut_ptr().drop_in_place();
            });
        }
    }
}

#[cfg(all(test, not(loom)))]
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
        let chan = TrickyPipe::<UnSerStruct>::new(4);
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
        let chan = TrickyPipe::<UnSerStruct>::new(4);
        let tx = chan.sender();
        let rx = chan.receiver().unwrap();
        drop(rx);
        assert_eq!(tx.try_reserve().unwrap_err(), TrySendError::Closed,);
    }

    #[test]
    fn normal_closed_tx() {
        let chan = TrickyPipe::<UnSerStruct>::new(4);
        let tx = chan.sender();
        let rx = chan.receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed,);
    }

    #[test]
    fn normal_closed_cloned_tx() {
        let chan = TrickyPipe::<UnSerStruct>::new(4);
        let tx1 = chan.sender();
        let tx2 = tx1.clone();
        let rx = chan.receiver().unwrap();
        drop(tx1);
        drop(tx2);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed,);
    }

    #[test]
    fn ser_smoke() {
        let chan = TrickyPipe::<SerStruct>::new(4);
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
        let chan = TrickyPipe::<SerStruct>::new(4);
        let tx = chan.sender();
        let rx = chan.ser_receiver().unwrap();
        drop(rx);
        assert_eq!(tx.try_reserve().unwrap_err(), TrySendError::Closed);
    }

    #[test]
    fn ser_closed_tx() {
        let chan = TrickyPipe::<SerStruct>::new(4);
        let tx = chan.sender();
        let rx = chan.ser_receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    }

    #[test]
    fn ser_closed_cloned_tx() {
        let chan = TrickyPipe::<SerStruct>::new(4);
        let tx1 = chan.sender();
        let tx2 = tx1.clone();
        let rx = chan.ser_receiver().unwrap();
        drop(tx1);
        drop(tx2);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    }

    #[test]
    fn ser_ref_smoke() {
        let chan = TrickyPipe::<SerStruct>::new(4);
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
        let chan = TrickyPipe::<DeStruct>::new(4);
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
        let chan = TrickyPipe::<DeStruct>::new(4);
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
        let chan = TrickyPipe::<DeStruct>::new(4);
        let tx = chan.ser_sender();
        let rx = chan.receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    }

    #[test]
    fn deser_closed_cloned_tx() {
        let chan = TrickyPipe::<DeStruct>::new(4);
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
        let chan = TrickyPipe::<SerDeStruct>::new(4);
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

#[cfg(test)]
mod loom {
    use super::*;
    use crate::loom;

    #[test]
    fn elements_dropped() {
        loom::model(|| {
            let chan = TrickyPipe::<loom::alloc::Track<usize>>::new(4);

            let rx = chan.receiver().expect("can't get rx");
            let tx = chan.sender();
            let thread = loom::thread::spawn(move || {
                tx.try_reserve()
                    .expect("reserve 1")
                    .send(loom::alloc::Track::new(1));

                tx.try_reserve()
                    .expect("reserve 2")
                    .send(loom::alloc::Track::new(2));

                tx.try_reserve()
                    .expect("reserve 3")
                    .send(loom::alloc::Track::new(3));
            });

            let item1 = loom::future::block_on(rx.recv()).expect("recv 1");
            assert_eq!(item1.get_ref(), &1);
            thread.join().expect("thread shouldn't panic");

            drop(rx);
            drop(chan);
        })
    }
}
