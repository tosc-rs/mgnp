use super::*;
use crate::loom;
use serde::Deserialize;

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct SerStruct {
    pub(crate) a: u8,
    pub(crate) b: i16,
    pub(crate) c: u32,
    pub(crate) d: String,
}

#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct DeStruct {
    pub(crate) a: u8,
    pub(crate) b: i16,
    pub(crate) c: u32,
    pub(crate) d: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct SerDeStruct {
    pub(crate) a: u8,
    pub(crate) b: i16,
    pub(crate) c: u32,
    pub(crate) d: String,
}

#[derive(Debug, PartialEq)]
pub(crate) struct UnSerStruct {
    pub(crate) a: u8,
    pub(crate) b: i16,
    pub(crate) c: u32,
    pub(crate) d: String,
}

#[test]
fn normal_smoke() {
    loom::model(|| {
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
    })
}

#[test]
fn normal_closed_rx() {
    loom::model(|| {
        let chan = TrickyPipe::<UnSerStruct>::new(4);
        let tx = chan.sender();
        let rx = chan.receiver().unwrap();
        drop(rx);
        assert_eq!(tx.try_reserve().unwrap_err(), TrySendError::Closed,);
    });
}

#[test]
fn normal_closed_tx() {
    loom::model(|| {
        let chan = TrickyPipe::<UnSerStruct>::new(4);
        let tx = chan.sender();
        let rx = chan.receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed,);
    });
}

#[test]
fn normal_closed_cloned_tx() {
    loom::model(|| {
        let chan = TrickyPipe::<UnSerStruct>::new(4);
        let tx1 = chan.sender();
        let tx2 = tx1.clone();
        let rx = chan.receiver().unwrap();
        drop(tx1);
        drop(tx2);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed,);
    });
}

#[test]
fn ser_smoke() {
    loom::model(|| {
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
    });
}

#[test]
fn ser_closed_rx() {
    loom::model(|| {
        let chan = TrickyPipe::<SerStruct>::new(4);
        let tx = chan.sender();
        let rx = chan.ser_receiver().unwrap();
        drop(rx);
        assert_eq!(tx.try_reserve().unwrap_err(), TrySendError::Closed);
    });
}

#[test]
fn ser_closed_tx() {
    loom::model(|| {
        let chan = TrickyPipe::<SerStruct>::new(4);
        let tx = chan.sender();
        let rx = chan.ser_receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    });
}

#[test]
fn ser_closed_cloned_tx() {
    loom::model(|| {
        let chan = TrickyPipe::<SerStruct>::new(4);
        let tx1 = chan.sender();
        let tx2 = tx1.clone();
        let rx = chan.ser_receiver().unwrap();
        drop(tx1);
        drop(tx2);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    });
}

#[test]
fn ser_ref_smoke() {
    loom::model(|| {
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
    });
}

#[test]
fn deser_smoke() {
    loom::model(|| {
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
    });
}

#[test]
fn deser_closed_rx() {
    loom::model(|| {
        let chan = TrickyPipe::<DeStruct>::new(4);
        let tx = chan.ser_sender();
        let rx = chan.receiver().unwrap();
        drop(rx);
        let res = tx.try_send([240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]);
        assert_eq!(
            res.unwrap_err(),
            SerTrySendError::Send(TrySendError::Closed),
        );
    });
}

#[test]
fn deser_closed_tx() {
    loom::model(|| {
        let chan = TrickyPipe::<DeStruct>::new(4);
        let tx = chan.ser_sender();
        let rx = chan.receiver().unwrap();
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    });
}

#[test]
fn deser_closed_cloned_tx() {
    loom::model(|| {
        let chan = TrickyPipe::<DeStruct>::new(4);
        let tx1 = chan.ser_sender();
        let tx2 = tx1.clone();
        let rx = chan.receiver().unwrap();
        drop(tx1);
        drop(tx2);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Closed);
    });
}

#[test]
fn deser_ser_smoke() {
    // Ideally the "serialize on both sides" case would just be a BBQueue or
    // some other kind of framed byte pipe thingy, but we should make sure
    // it works anyway i guess...
    loom::model(|| {
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
    });
}

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
