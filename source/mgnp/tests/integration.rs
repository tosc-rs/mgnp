#![cfg(feature = "alloc")]
mod support;
use mgnp::Interface;
use support::*;

use std::sync::Arc;
use tricky_pipe::{mpsc, oneshot, serbox};

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
            let (_, mut machine) = Interface::new::<_, _, { mgnp::DEFAULT_MAX_CONNS }>(
                wire1,
                registry1,
                mpsc::TrickyPipe::new(8),
            );

            tokio::select! {
                res = machine.run() => {
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

    let (iface, mut machine) = Interface::new::<_, _, { mgnp::DEFAULT_MAX_CONNS }>(
        wire2,
        TestRegistry::default(),
        mpsc::TrickyPipe::new(8),
    );
    let local = tokio::spawn({
        let test_done = test_done.clone();
        async move {
            tokio::select! {
            res = machine.run() => {
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

    let mut connector =
        iface.connector::<HelloWorldService>(serbox::Sharer::new(), oneshot::Receiver::new());

    let chan = connector
        .connect(hello_world_id(), (), mgnp::connector::Channels::new(8))
        .await
        .expect("connection should be established");
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
