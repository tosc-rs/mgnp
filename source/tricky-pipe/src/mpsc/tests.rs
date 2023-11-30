use super::*;
use crate::loom::{self, future, thread};
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

mod trait_impls {
    use super::*;

    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}
    fn assert_unpin<T: Unpin>() {}

    #[test]
    fn receiver() {
        assert_send::<Receiver<UnSerStruct>>();
        assert_sync::<Receiver<UnSerStruct>>();
        assert_unpin::<Receiver<UnSerStruct>>();
    }

    #[test]
    fn sender() {
        assert_send::<Sender<UnSerStruct>>();
        assert_sync::<Sender<UnSerStruct>>();
        assert_unpin::<Sender<UnSerStruct>>();
    }

    #[test]
    fn ser_receiver() {
        assert_send::<SerReceiver>();
        assert_sync::<SerReceiver>();
        assert_unpin::<SerReceiver>();
    }

    #[test]
    fn deser_sender() {
        assert_send::<DeserSender>();
        assert_sync::<DeserSender>();
        assert_unpin::<DeserSender>();
    }

    #[test]
    fn recv_future() {
        assert_send::<Recv<'_, UnSerStruct>>();
        assert_sync::<Recv<'_, UnSerStruct>>();
        assert_unpin::<Recv<'_, UnSerStruct>>();
    }

    #[test]
    fn ser_recv_future() {
        assert_send::<SerRecv<'_>>();
        assert_sync::<SerRecv<'_>>();
        assert_unpin::<SerRecv<'_>>();
    }

    #[test]
    fn ser_recv_ref() {
        assert_send::<SerRecvRef<'_>>();
        assert_sync::<SerRecvRef<'_>>();
        assert_unpin::<SerRecvRef<'_>>();
    }

    #[test]
    fn permit() {
        assert_send::<Permit<'_, UnSerStruct, ()>>();
        assert_sync::<Permit<'_, UnSerStruct, ()>>();
        assert_unpin::<Permit<'_, UnSerStruct, ()>>();
    }

    #[test]
    fn ser_permit() {
        assert_send::<SerPermit<'_, ()>>();
        assert_sync::<SerPermit<'_, ()>>();
        assert_unpin::<SerPermit<'_, ()>>();
    }
}

mod single_threaded {
    use super::*;

    #[test]
    fn normal_smoke() {
        loom::model(|| {
            let chan = TrickyPipe::<UnSerStruct>::new(4);
            let tx = test_dbg!(chan.sender());
            let rx = test_dbg!(chan.receiver().unwrap());
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
            let tx = test_dbg!(chan.sender());
            let rx = test_dbg!(chan.receiver().unwrap());
            drop(rx);
            assert_eq!(
                tx.try_reserve().unwrap_err(),
                TrySendError::Disconnected(()),
            );
        });
    }

    #[test]
    fn normal_closed_tx() {
        loom::model(|| {
            let chan = TrickyPipe::<UnSerStruct>::new(4);
            let tx = test_dbg!(chan.sender());
            let rx = test_dbg!(chan.receiver().unwrap());
            drop(chan);
            drop(tx);
            assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Disconnected,);
        });
    }

    #[test]
    fn normal_closed_cloned_tx() {
        loom::model(|| {
            let chan = TrickyPipe::<UnSerStruct>::new(4);
            let tx1 = test_dbg!(chan.sender());
            let tx2 = test_dbg!(tx1.clone());
            let rx = test_dbg!(chan.receiver().unwrap());
            drop(chan);
            drop(tx1);
            drop(tx2);
            assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Disconnected,);
        });
    }

    #[test]
    fn ser_smoke() {
        loom::model(|| {
            let chan = TrickyPipe::<SerStruct>::new(4);
            let tx = test_dbg!(chan.sender());
            let rx = test_dbg!(chan.ser_receiver()).unwrap();
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
            let tx = test_dbg!(chan.sender());
            let rx = test_dbg!(chan.ser_receiver()).unwrap();
            drop(rx);
            assert_eq!(
                tx.try_reserve().unwrap_err(),
                TrySendError::Disconnected(())
            );
        });
    }

    #[test]
    fn ser_closed_tx() {
        loom::model(|| {
            let chan = TrickyPipe::<SerStruct>::new(4);
            let tx = test_dbg!(chan.sender());
            let rx = test_dbg!(chan.ser_receiver()).unwrap();
            drop(chan);
            drop(tx);
            assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Disconnected);
        });
    }

    #[test]
    fn ser_closed_cloned_tx() {
        loom::model(|| {
            let chan = TrickyPipe::<SerStruct>::new(4);
            let tx1 = chan.sender();
            let tx2 = tx1.clone();
            let rx = test_dbg!(chan.ser_receiver()).unwrap();
            drop(chan);
            drop(tx1);
            drop(tx2);
            assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Disconnected);
        });
    }

    #[test]
    fn ser_ref_smoke() {
        loom::model(|| {
            let chan = TrickyPipe::<SerStruct>::new(4);
            let tx = test_dbg!(chan.sender());
            let rx = test_dbg!(chan.ser_receiver()).unwrap();
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
            let tx = test_dbg!(chan.deser_sender());
            let rx = test_dbg!(chan.receiver()).unwrap();
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
            let tx = test_dbg!(chan.deser_sender());
            let rx = test_dbg!(chan.receiver()).unwrap();
            drop(chan);
            drop(rx);
            let res = tx.try_send([240, 223, 93, 160, 141, 6, 5, 104, 101, 108, 108, 111]);
            assert_eq!(res.unwrap_err(), SerTrySendError::Disconnected,);
        });
    }

    #[test]
    fn deser_closed_tx() {
        loom::model(|| {
            let chan = TrickyPipe::<DeStruct>::new(4);
            let tx = test_dbg!(chan.deser_sender());
            let rx = test_dbg!(chan.receiver()).unwrap();
            drop(chan);
            drop(tx);
            assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Disconnected);
        });
    }

    #[test]
    fn deser_closed_cloned_tx() {
        loom::model(|| {
            let chan = TrickyPipe::<DeStruct>::new(4);
            let tx1 = test_dbg!(chan.deser_sender());
            let tx2 = tx1.clone();
            let rx = test_dbg!(chan.receiver()).unwrap();
            drop(chan);
            drop(tx1);
            drop(tx2);
            assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Disconnected);
        });
    }

    #[test]
    fn deser_ser_smoke() {
        // Ideally the "serialize on both sides" case would just be a BBQueue or
        // some other kind of framed byte pipe thingy, but we should make sure
        // it works anyway i guess...
        loom::model(|| {
            let chan = TrickyPipe::<SerDeStruct>::new(4);
            let tx = test_dbg!(chan.deser_sender());
            let rx = test_dbg!(chan.ser_receiver()).unwrap();
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
}

#[test]
fn elements_dropped() {
    loom::model(|| {
        let chan = TrickyPipe::<loom::alloc::Track<usize>>::new(4);

        let rx = test_dbg!(chan.receiver()).expect("can't get rx");
        let tx = test_dbg!(chan.sender());
        let thread = thread::spawn(move || {
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

        let item1 = future::block_on(rx.recv()).expect("recv 1");
        assert_eq!(item1.get_ref(), &1);
        thread.join().expect("thread shouldn't panic");

        drop(rx);
        drop(chan);
    })
}

#[test]
fn spsc_try_send_in_capacity() {
    const SENDS: usize = if cfg!(loom) || cfg!(miri) { 4 } else { 32 };

    loom::model(|| {
        let (rx, tx) = {
            let chan = TrickyPipe::<loom::alloc::Track<usize>>::new(SENDS as u8);
            let rx = test_dbg!(chan.receiver()).expect("can't get rx");
            let tx = test_dbg!(chan.sender());
            (rx, tx)
        };
        let thread = thread::spawn(move || {
            for i in 0..SENDS {
                tx.try_reserve()
                    .expect("try_reserve")
                    .send(loom::alloc::Track::new(i));
            }
        });

        future::block_on(async move {
            let mut i = 0;
            while let Ok(msg) = test_dbg!(rx.recv().await) {
                assert_eq!(msg.get_ref(), &i);
                i += 1;
            }
            assert_eq!(i, SENDS);
        });

        thread.join().unwrap();
    })
}

#[test]
fn spsc_send() {
    const SENDS: usize = if cfg!(loom) || cfg!(miri) { 8 } else { 32 };

    loom::model(|| {
        let (rx, tx) = {
            let chan = TrickyPipe::<loom::alloc::Track<usize>>::new((SENDS / 2) as u8);
            let rx = test_dbg!(chan.receiver()).expect("can't get rx");
            let tx = test_dbg!(chan.sender());
            (rx, tx)
        };

        let thread = thread::spawn(do_tx(SENDS, 0, tx));

        future::block_on(async move {
            let mut i = 0;
            while let Ok(msg) = rx.recv().await {
                assert_eq!(msg.get_ref(), &i);
                i += 1;
            }
            assert_eq!(i, SENDS);
        });

        thread.join().unwrap();
    })
}

#[test]
fn downcast_send() {
    const SENDS: u8 = 2;

    loom::model(|| {
        let (rx, tx) = {
            let chan = TrickyPipe::<loom::alloc::Track<u8>>::new(SENDS);
            let rx = test_dbg!(chan.receiver()).expect("can't get rx");
            let tx = test_dbg!(chan.sender().into_erased());
            (rx, tx)
        };

        let thread = thread::spawn(move || {
            future::block_on(async move {
                for i in 0..SENDS {
                    let permit = tx.reserve().await.unwrap();
                    let permit = permit
                        .downcast::<u8>()
                        .expect_err("wrong type, should not downcast");
                    permit
                        .downcast()
                        .expect("correct type should downcast")
                        .send(loom::alloc::Track::new(i));
                }
            })
        });

        future::block_on(async move {
            let mut i = 0;
            while let Ok(msg) = rx.recv().await {
                assert_eq!(msg.get_ref(), &i);
                i += 1;
            }
            assert_eq!(i, SENDS);
        });

        thread.join().unwrap();
    })
}

#[test]
fn mpsc_send() {
    // try not to make the test run for > 300 seconds under loom/miri...
    const TX1_SENDS: usize = if cfg!(loom) || cfg!(miri) { 2 } else { 16 };
    const TX2_SENDS: usize = if cfg!(loom) || cfg!(miri) { 1 } else { 16 };
    const SENDS: usize = TX1_SENDS + TX2_SENDS;
    const CAPACITY: u8 = if cfg!(loom) || cfg!(miri) { 2 } else { 32 };

    loom::model(|| {
        let chan = TrickyPipe::<loom::alloc::Track<usize>>::new(CAPACITY);

        let rx = test_dbg!(chan.receiver()).expect("can't get rx");
        let tx1 = chan.sender();
        let tx2 = chan.sender();
        // drop the channel now so that the channel can be tx-closed.
        drop(chan);

        let t1 = thread::spawn(do_tx(TX1_SENDS, 0, tx1));
        let t2 = thread::spawn(do_tx(TX2_SENDS, TX1_SENDS, tx2));

        let recvs = future::block_on(async move {
            let mut recvs = std::collections::BTreeSet::new();
            while let Ok(msg) = rx.recv().await {
                let msg = msg.into_inner();
                tracing::info!(received = msg);
                assert!(
                    recvs.insert(msg),
                    "each message should only have been received once\nmessage: {msg}\nreceived: {recvs:?}"
                );
            }
            recvs
        });

        t1.join().unwrap();
        t2.join().unwrap();

        for msg in 0..SENDS {
            assert!(
                recvs.contains(&msg),
                "didn't receive {}\nreceived: {recvs:?}",
                msg
            );
        }
    })
}

#[test]
// this would probably run for 1000 years under loom...
#[cfg_attr(any(loom, miri), ignore)]
fn stress_mpsc_send() {
    const TX1_SENDS: usize = 64;
    const TX2_SENDS: usize = 64;
    const SENDS: usize = TX1_SENDS + TX2_SENDS;
    const CAPACITY: u8 = 8;

    loom::model(|| {
        let chan = TrickyPipe::<loom::alloc::Track<usize>>::new(CAPACITY);

        let rx = test_dbg!(chan.receiver()).expect("can't get rx");
        let tx1 = chan.sender();
        let tx2 = chan.sender();
        // drop the channel now so that the channel can be tx-closed.
        drop(chan);

        let t1 = thread::spawn(do_tx(TX1_SENDS, 0, tx1));
        let t2 = thread::spawn(do_tx(TX2_SENDS, TX1_SENDS, tx2));

        let recvs = future::block_on(async move {
            let mut recvs = std::collections::BTreeSet::new();
            while let Ok(msg) = rx.recv().await {
                let msg = msg.into_inner();
                tracing::info!(received = msg);
                assert!(
                    recvs.insert(msg),
                    "each message should only have been received once\nmessage: {msg}\nreceived: {recvs:?}"
                );
            }
            recvs
        });

        t1.join().unwrap();
        t2.join().unwrap();

        for msg in 0..SENDS {
            assert!(
                recvs.contains(&msg),
                "didn't receive {}\nreceived: {recvs:?}",
                msg
            );
        }
    })
}

#[test]
fn rx_closes_error() {
    const CAPACITY: u8 = 2;

    loom::model(|| {
        let chan = TrickyPipe::<loom::alloc::Track<usize>, &'static str>::new(CAPACITY);

        let rx = test_dbg!(chan.receiver()).expect("can't get rx");
        let tx = chan.sender();

        rx.close_with_error("fake rx error");

        let t1 = thread::spawn(move || {
            future::block_on(async move {
                let err = test_dbg!(tx.send(loom::alloc::Track::new(1)).await.unwrap_err());
                match err {
                    SendError::Error { error, .. } => assert_eq!(error, "fake rx error"),
                    err => panic!("expected SendError::Error, got {:?}", err),
                }
            })
        });

        t1.join().unwrap();
    })
}

#[test]
fn tx_closes_error() {
    const CAPACITY: u8 = 2;

    loom::model(|| {
        let chan = TrickyPipe::<loom::alloc::Track<usize>, &'static str>::new(CAPACITY);

        let rx = test_dbg!(chan.receiver()).expect("can't get rx");
        let tx1 = chan.sender();
        let tx2 = chan.sender();

        let t1 = thread::spawn(move || {
            tx1.close_with_error("fake tx1 error");
        });

        let t2 = thread::spawn(move || {
            tx2.close_with_error("fake tx2 error");
        });

        future::block_on(async move {
            let err = test_dbg!(rx.recv().await).unwrap_err();
            assert!(matches!(
                err,
                RecvError::Error("fake tx1 error") | RecvError::Error("fake tx2 error")
            ))
        });

        t1.join().unwrap();
        t2.join().unwrap();
    })
}

fn do_tx(
    sends: usize,
    offset: usize,
    tx: Sender<loom::alloc::Track<usize>>,
) -> impl FnOnce() + Send {
    move || {
        future::block_on(async move {
            for i in offset..offset + sends {
                test_dbg!(tx.reserve().await)
                    .expect("channel shouldn't be closed")
                    .send(loom::alloc::Track::new(i));
            }
        });
    }
}
