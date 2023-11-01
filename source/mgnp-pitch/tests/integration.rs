mod support;
use mgnp_pitch::{Interface, OutboundConnect};
use support::*;

use std::sync::Arc;

#[tokio::test]
async fn basically_works() {
    trace_init();

    let test_done = Arc::new(tokio::sync::Notify::new());

    let registry1 = TestRegistry::default();
    registry1.spawn_hello_world();

    let (wire1, wire2) = TestWire::new();
    let remote = tokio::spawn({
        let test_done = test_done.clone();
        async move {
            let mut iface =
                Interface::<_, _, { mgnp_pitch::DEFAULT_MAX_CONNS }>::new(wire1, registry1);

            tokio::select! {
                res = iface.run(futures::stream::pending()) => {
                    tracing::info!(?res, "remote interface run loop terminated!");
                    // the remote may be terminated by an interface error, since
                    // the wire will be dropped.
                },
                _ = test_done.notified() => {
                    tracing::debug!("test done, remote shutting down...");
                },
            }
        }
        .instrument(tracing::info_span!("remote"))
    });

    let (conn_tx, conn_rx) = tokio::sync::mpsc::channel(4);
    let local = tokio::spawn({
        let test_done = test_done.clone();
        async move {
            let mut iface = Interface::<_, _, { mgnp_pitch::DEFAULT_MAX_CONNS }>::new(
                wire2,
                TestRegistry::default(),
            );
            let conns = tokio_stream::wrappers::ReceiverStream::new(conn_rx);
            tokio::select! {
            res = iface.run(conns) => {
                tracing::info!(?res, "local interface run loop terminated!");
                // the local task may be terminated by an interface error, since
                // the wire will be dropped.
            },
            _ = test_done.notified() => {
                tracing::debug!("test done, local shutting down...");
            },
            }
        }
        .instrument(tracing::info_span!("local"))
    });

    let (ser_chan, chan) = make_bidis::<HelloWorldResponse, HelloWorldRequest>(8);
    conn_tx
        .send(OutboundConnect::new(hello_world_id(), &[], ser_chan))
        .await
        .unwrap();

    chan.tx()
        .send(HelloWorldRequest {
            hello: "hello".to_string(),
        })
        .await
        .expect("send request");
    let rsp = chan.rx().recv().await;
    assert_eq!(
        rsp,
        Some(HelloWorldResponse {
            world: "world".to_string()
        })
    );

    // complete the test and wait for both the "local" and "remote" interface
    // tasks to complete, so we can ensure they didn't panic.
    test_done.notify_waiters();
    local.await.expect("local interface task should not panic");
    remote
        .await
        .expect("remote interface task should not panic");
}
