use super::{
    channel_core::{Core, CoreVtable, ErasedPipe, ErasedSlice},
    *,
};

pub struct StaticTrickyPipe<T: 'static, const CAPACITY: usize> {
    elements: [Cell<T>; CAPACITY],
    core: Core,
}

impl<T: 'static, const CAPACITY: usize> StaticTrickyPipe<T, CAPACITY> {
    const EMPTY_CELL: Cell<T> = UnsafeCell::new(MaybeUninit::uninit());

    pub const fn new() -> Self {
        assert!(CAPACITY.is_power_of_two());
        assert!(CAPACITY <= Self::MAX_CAPACITY);
        Self {
            core: Core::new(CAPACITY as u8),
            elements: [Self::EMPTY_CELL; CAPACITY],
        }
    }

    pub const MAX_CAPACITY: usize = channel_core::MAX_CAPACITY;

    const CORE_VTABLE: &'static CoreVtable = &CoreVtable {
        get_core: Self::get_core,
        get_elems: Self::get_elems,
        clone: Self::erased_clone,
        drop: Self::erased_drop,
    };

    fn erased(&'static self) -> ErasedPipe {
        unsafe { ErasedPipe::new(self as *const _ as *const (), Self::CORE_VTABLE) }
    }

    fn typed(&'static self) -> TypedPipe<T> {
        unsafe { self.erased().typed() }
    }

    pub fn receiver(&'static self) -> Option<Receiver<T>> {
        self.core.try_claim_rx()?;

        Some(Receiver { pipe: self.typed() })
    }

    pub fn sender(&'static self) -> Sender<T> {
        self.core.add_tx();
        Sender { pipe: self.typed() }
    }

    fn get_core(ptr: *const ()) -> *const Core {
        unsafe {
            let ptr = ptr.cast::<Self>();
            ptr::addr_of!((*ptr).core)
        }
    }

    fn get_elems(ptr: *const ()) -> ErasedSlice {
        unsafe {
            let ptr = ptr.cast::<Self>();
            ErasedSlice::erase(&(*ptr).elements)
        }
    }

    fn erased_clone(_: *const ()) {
        // nop, cloning a `&'static T` nops
    }

    fn erased_drop(_: *const ()) {}
}

impl<T, const CAPACITY: usize> StaticTrickyPipe<T, CAPACITY>
where
    T: Serialize + 'static,
{
    pub fn ser_receiver(&'static self) -> Option<SerReceiver> {
        self.core.try_claim_rx()?;

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

impl<T, const CAPACITY: usize> StaticTrickyPipe<T, CAPACITY>
where
    T: DeserializeOwned + 'static,
{
    pub fn ser_sender(&'static self) -> SerSender {
        self.core.add_tx();
        SerSender {
            pipe: self.erased(),
            vtable: Self::DESER_VTABLE,
        }
    }

    const DESER_VTABLE: &'static DeserVtable = &DeserVtable::new::<T>();
}

unsafe impl<T: Send, const CAPACITY: usize> Send for StaticTrickyPipe<T, CAPACITY> {}
unsafe impl<T: Send, const CAPACITY: usize> Sync for StaticTrickyPipe<T, CAPACITY> {}

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
        static CHAN: StaticTrickyPipe<UnSerStruct, 4> = StaticTrickyPipe::new();
        let tx = CHAN.sender();
        let rx = CHAN.receiver().unwrap();
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
    fn smaller_capacities_are_smaller() {
        use core::mem::size_of_val;
        static BIG: StaticTrickyPipe<SerStruct, { channel_core::MAX_CAPACITY }> =
            StaticTrickyPipe::new();

        static LITTLE: StaticTrickyPipe<SerStruct, 2> = StaticTrickyPipe::new();
        let big_size = dbg!(size_of_val(&BIG));
        let little_size = dbg!(size_of_val(&LITTLE));
        assert!(little_size < big_size);
    }

    #[test]
    fn normal_closed_rx() {
        static CHAN: StaticTrickyPipe<UnSerStruct, 4> = StaticTrickyPipe::new();
        let tx = CHAN.sender();
        let rx = CHAN.receiver().unwrap();
        drop(rx);
        assert_eq!(tx.try_reserve().unwrap_err(), TrySendError::Closed,);
    }

    #[test]
    fn normal_closed_tx() {
        static CHAN: StaticTrickyPipe<UnSerStruct, 4> = StaticTrickyPipe::new();
        let tx = CHAN.sender();
        let rx = CHAN.receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed,);
    }

    #[test]
    fn normal_closed_cloned_tx() {
        static CHAN: StaticTrickyPipe<UnSerStruct, 4> = StaticTrickyPipe::new();
        let tx1 = CHAN.sender();
        let tx2 = tx1.clone();
        let rx = CHAN.receiver().unwrap();
        drop(tx1);
        drop(tx2);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed,);
    }

    #[test]
    fn ser_smoke() {
        static CHAN: StaticTrickyPipe<SerStruct, 4> = StaticTrickyPipe::new();
        let tx = CHAN.sender();
        let rx = CHAN.ser_receiver().unwrap();
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
        static CHAN: StaticTrickyPipe<SerStruct, 4> = StaticTrickyPipe::new();
        let tx = CHAN.sender();
        let rx = CHAN.ser_receiver().unwrap();
        drop(rx);
        assert_eq!(tx.try_reserve().unwrap_err(), TrySendError::Closed);
    }

    #[test]
    fn ser_closed_tx() {
        static CHAN: StaticTrickyPipe<SerStruct, 4> = StaticTrickyPipe::new();
        let tx = CHAN.sender();
        let rx = CHAN.ser_receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    }

    #[test]
    fn ser_closed_cloned_tx() {
        static CHAN: StaticTrickyPipe<SerStruct, 4> = StaticTrickyPipe::new();
        let tx1 = CHAN.sender();
        let tx2 = tx1.clone();
        let rx = CHAN.ser_receiver().unwrap();
        drop(tx1);
        drop(tx2);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    }

    #[test]
    fn ser_ref_smoke() {
        static CHAN: StaticTrickyPipe<SerStruct, 4> = StaticTrickyPipe::new();
        let tx = CHAN.sender();
        let rx = CHAN.ser_receiver().unwrap();
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
        static CHAN: StaticTrickyPipe<DeStruct, 4> = StaticTrickyPipe::new();
        let tx = CHAN.ser_sender();
        let rx = CHAN.receiver().unwrap();
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
        static CHAN: StaticTrickyPipe<DeStruct, 4> = StaticTrickyPipe::new();
        let tx = CHAN.ser_sender();
        let rx = CHAN.receiver().unwrap();
        drop(rx);
        let res = tx.try_send([240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]);
        assert_eq!(
            res.unwrap_err(),
            SerTrySendError::Send(TrySendError::Closed),
        );
    }

    #[test]
    fn deser_closed_tx() {
        static CHAN: StaticTrickyPipe<DeStruct, 4> = StaticTrickyPipe::new();
        let tx = CHAN.ser_sender();
        let rx = CHAN.receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    }

    #[test]
    fn deser_closed_cloned_tx() {
        static CHAN: StaticTrickyPipe<DeStruct, 4> = StaticTrickyPipe::new();
        let tx1 = CHAN.ser_sender();
        let tx2 = tx1.clone();
        let rx = CHAN.receiver().unwrap();
        drop(tx1);
        drop(tx2);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    }

    #[test]
    fn deser_ser_smoke() {
        // Ideally the "serialize on both sides" case would just be a BBQueue or
        // some other kind of framed byte pipe thingy, but we should make sure
        // it works anyway i guess...
        static CHAN: StaticTrickyPipe<SerDeStruct, 4> = StaticTrickyPipe::new();
        let tx = CHAN.ser_sender();
        let rx = CHAN.ser_receiver().unwrap();
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
