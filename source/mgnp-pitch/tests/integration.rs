mod support;
use mgnp_pitch::{Interface, OutboundConnect};
use support::*;

#[tokio::test]
async fn basically_works() {
    trace_init();

    let registry1 = TestRegistry::default();
    registry1.spawn_hello_world();

    let (wire1, wire2) = TestWire::new();

    let remote = tokio::spawn(
        async move {
            let mut iface =
                Interface::<_, _, _, { mgnp_pitch::DEFAULT_MAX_CONNS }>::new(wire1, registry1);
            iface
                .run(futures::stream::pending())
                .await
                .expect("interface error!");
        }
        .instrument(tracing::info_span!("remote")),
    );

    let (conn_tx, conn_rx) = tokio::sync::mpsc::channel(4);
    let local = tokio::spawn(
        async move {
            let mut iface = Interface::<_, _, _, { mgnp_pitch::DEFAULT_MAX_CONNS }>::new(
                wire2,
                TestRegistry::default(),
            );
            let conns = tokio_stream::wrappers::ReceiverStream::new(conn_rx);
            iface.run(conns).await.expect("local interface error!")
        }
        .instrument(tracing::info_span!("remote")),
    );

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
}
