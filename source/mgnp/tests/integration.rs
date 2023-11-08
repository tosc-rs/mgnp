#![cfg(feature = "alloc")]
mod support;
use mgnp::Interface;
use support::*;

use std::sync::Arc;
use tricky_pipe::{mpsc, oneshot, serbox};

#[tokio::test]
async fn basically_works() {
    let remote_registry: TestRegistry = TestRegistry::default();
    remote_registry.spawn_hello_world();

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry);

    let mut connector = fixture
        .local_iface()
        .connector::<HelloWorldService>(serbox::Sharer::new(), oneshot::Receiver::new());

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

    fixture.finish_test().await;
}

#[tokio::test]
async fn hellos_work() {
    let remote_registry: TestRegistry = TestRegistry::default();
    remote_registry.spawn_hello_with_hello();

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry);

    let mut connector = fixture
        .local_iface()
        .connector::<HelloWithHelloService>(serbox::Sharer::new(), oneshot::Receiver::new());

    let chan = connector
        .connect(
            hello_with_hello_id(),
            HelloHello {
                hello: "hello".into(),
            },
            mgnp::connector::Channels::new(8),
        )
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

    fixture.finish_test().await;
}
