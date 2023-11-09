#![cfg(feature = "alloc")]
mod support;
use mgnp::{
    connector::{self, ConnectError},
    message::Nak,
    registry::Identity,
    tricky_pipe::{oneshot, serbox},
};
use support::*;
use svcs::{HelloWorldRequest, HelloWorldResponse};

#[tokio::test]
async fn basically_works() {
    let remote_registry: TestRegistry = TestRegistry::default();
    remote_registry.spawn_hello_world();

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry);

    let mut connector = fixture
        .local_iface()
        .connector::<svcs::HelloWorld>(serbox::Sharer::new(), oneshot::Receiver::new());

    let chan = connector
        .connect("hello-world", (), connector::Channels::new(8))
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
        .connector::<svcs::HelloWithHello>(serbox::Sharer::new(), oneshot::Receiver::new());

    let chan = connector
        .connect(
            "hello-hello",
            svcs::HelloHello {
                hello: "hello".into(),
            },
            connector::Channels::new(8),
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

#[tokio::test]
async fn nak_bad_hello() {
    let remote_registry: TestRegistry = TestRegistry::default();
    remote_registry.spawn_hello_with_hello();

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry);

    let mut connector = fixture
        .local_iface()
        .connector::<svcs::HelloWithHello>(serbox::Sharer::new(), oneshot::Receiver::new());

    // establish a good connection with a valid hello
    let chan = connector
        .connect(
            "hello-hello",
            svcs::HelloHello {
                hello: "hello".into(),
            },
            connector::Channels::new(8),
        )
        .await
        .expect("connection should be established");

    // now try to connect again with an invalid hello
    let err = connector
        .connect(
            "hello-hello",
            svcs::HelloHello {
                hello: "goodbye".into(),
            },
            connector::Channels::new(8),
        )
        .await
        .expect_err("connection with wrong hello message should be NAK'd");
    assert_eq!(err, ConnectError::Nak(Nak::Rejected));

    // the good connection should stil lwork
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
async fn mux_single_service() {
    let remote_registry: TestRegistry = TestRegistry::default();
    remote_registry.spawn_hello_world();

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry);

    let mut connector = fixture
        .local_iface()
        .connector::<svcs::HelloWorld>(serbox::Sharer::new(), oneshot::Receiver::new());

    let chan1 = connector
        .connect("hello-world", (), connector::Channels::new(8))
        .await
        .expect("connection should be established");

    let chan2 = connector
        .connect("hello-world", (), connector::Channels::new(8))
        .await
        .expect("connection should be established");

    tokio::try_join! {
        chan1.tx().send(HelloWorldRequest {
            hello: "hello".to_string(),
        }),
        chan2.tx().send(HelloWorldRequest {
            hello: "hello".to_string(),
        })
    }
    .expect("send should work");

    let (rsp1, rsp2) = tokio::join! {
        chan1.rx().recv(),
        chan2.rx().recv(),
    };

    assert_eq!(
        rsp1,
        Some(HelloWorldResponse {
            world: "world".to_string()
        })
    );
    assert_eq!(
        rsp2,
        Some(HelloWorldResponse {
            world: "world".to_string()
        })
    );

    fixture.finish_test().await;
}

#[tokio::test]
async fn service_type_routing() {
    // remote comes up with NO services present...
    let remote_registry: TestRegistry = TestRegistry::default();

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry.clone());

    let iface = fixture.local_iface();

    // create connectors for both services
    let mut helloworld_connector =
        iface.connector::<svcs::HelloWorld>(serbox::Sharer::new(), oneshot::Receiver::new());
    let mut hellohello_connector =
        iface.connector::<svcs::HelloWithHello>(serbox::Sharer::new(), oneshot::Receiver::new());

    // attempts to initiate connections should fail when the remote services
    // don't exist
    let err = dbg!(
        helloworld_connector
            .connect("hello-world", (), connector::Channels::new(8))
            .await
    )
    .expect_err("HelloWorld connection should be NAKed when service is not present");
    assert_eq!(err, ConnectError::Nak(Nak::NotFound));

    let err = dbg!(
        hellohello_connector
            .connect(
                "hello-hello",
                svcs::HelloHello {
                    hello: "hello".into()
                },
                connector::Channels::new(8)
            )
            .await
    )
    .expect_err("HelloHello connection should be NAKed when service is not present");
    assert_eq!(err, ConnectError::Nak(Nak::NotFound));

    // add a service
    remote_registry.spawn_hello_world();

    // connecting to HelloHello should still fail
    let err = dbg!(
        hellohello_connector
            .connect(
                "hello-hello",
                svcs::HelloHello {
                    hello: "hello".into()
                },
                connector::Channels::new(8)
            )
            .await
    )
    .expect_err("HelloHello connection should be NAKed when service is not present");
    assert_eq!(err, ConnectError::Nak(Nak::NotFound));

    // ... but connecting to HelloWorld should succeed
    let helloworld_chan = helloworld_connector
        .connect("hello-world", (), connector::Channels::new(8))
        .await
        .expect("HelloWorld connection should be established");

    helloworld_chan
        .tx()
        .send(HelloWorldRequest {
            hello: "hello".to_string(),
        })
        .await
        .expect("send request");
    let rsp = helloworld_chan.rx().recv().await;
    assert_eq!(
        rsp,
        Some(HelloWorldResponse {
            world: "world".to_string()
        })
    );

    // add the other service
    remote_registry.spawn_hello_with_hello();

    let hellohello_chan = dbg!(
        hellohello_connector
            .connect(
                "hello-hello",
                svcs::HelloHello {
                    hello: "hello".into()
                },
                connector::Channels::new(8)
            )
            .await
    )
    .expect("HelloHello connection should succeed");

    hellohello_chan
        .tx()
        .send(HelloWorldRequest {
            hello: "hello".to_string(),
        })
        .await
        .expect("send request");

    helloworld_chan
        .tx()
        .send(HelloWorldRequest {
            hello: "hello".to_string(),
        })
        .await
        .expect("send request");

    let rsp = helloworld_chan.rx().recv().await;
    assert_eq!(
        rsp,
        Some(HelloWorldResponse {
            world: "world".to_string()
        })
    );

    let rsp = hellohello_chan.rx().recv().await;
    assert_eq!(
        rsp,
        Some(HelloWorldResponse {
            world: "world".to_string()
        })
    );
}

#[tokio::test]
async fn service_identity_routing() {
    // remote comes up with NO services present...
    let remote_registry: TestRegistry = TestRegistry::default();

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry.clone());

    let mut connector = fixture
        .local_iface()
        .connector::<svcs::HelloWorld>(serbox::Sharer::new(), oneshot::Receiver::new());

    // attempts to initiate connections should fail when the remote services
    // don't exist
    let err = dbg!(
        connector
            .connect("hello-world", (), connector::Channels::new(8))
            .await
    )
    .expect_err("service 'hello-world' connection should be NAKed when service is not present");
    assert_eq!(err, ConnectError::Nak(Nak::NotFound));

    let err = dbg!(
        connector
            .connect("hello-sf", (), connector::Channels::new(8))
            .await
    )
    .expect_err("service 'hello-sf' connection should be NAKed when service is not present");
    assert_eq!(err, ConnectError::Nak(Nak::NotFound));

    let err = dbg!(
        connector
            .connect("hello-universe", (), connector::Channels::new(8))
            .await
    )
    .expect_err("service 'hello-universe' connection should be NAKed when service is not present");
    assert_eq!(err, ConnectError::Nak(Nak::NotFound));

    // add the 'hello-sf' service
    let conns = remote_registry.add_service(Identity::from_name::<svcs::HelloWorld>("hello-sf"));
    tokio::spawn(svcs::serve_hello("hello sf", "san francisco", conns));

    // connecting to hello-world should still fail
    let err = dbg!(
        connector
            .connect("hello-world", (), connector::Channels::new(8))
            .await
    )
    .expect_err("service 'hello-world' connection should be NAKed when service is not present");
    assert_eq!(err, ConnectError::Nak(Nak::NotFound));

    // ... but connecting to 'hello-sf' should succeed
    let sf_conn = dbg!(
        connector
            .connect("hello-sf", (), connector::Channels::new(8))
            .await
    )
    .expect("service 'hello-sf' connection should be established");

    sf_conn
        .tx()
        .send(HelloWorldRequest {
            hello: "hello sf".to_string(),
        })
        .await
        .expect("send request");
    let rsp = sf_conn.rx().recv().await;
    assert_eq!(
        rsp,
        Some(HelloWorldResponse {
            world: "san francisco".to_string()
        })
    );

    // add the 'hello-universe' service
    let conns =
        remote_registry.add_service(Identity::from_name::<svcs::HelloWorld>("hello-universe"));
    tokio::spawn(svcs::serve_hello("hello universe", "universe", conns));

    let err = dbg!(
        connector
            .connect("hello-world", (), connector::Channels::new(8))
            .await
    )
    .expect_err("service 'hello-world' connection should be NAKed when service is not present");
    assert_eq!(err, ConnectError::Nak(Nak::NotFound));

    let uni_conn = dbg!(
        connector
            .connect("hello-universe", (), connector::Channels::new(8))
            .await
    )
    .expect("service 'hello-universe' connection should be established");

    let uni_req = uni_conn.tx().send(HelloWorldRequest {
        hello: "hello universe".to_string(),
    });
    let sf_req = sf_conn.tx().send(HelloWorldRequest {
        hello: "hello sf".to_string(),
    });

    tokio::try_join!(uni_req, sf_req).expect("both requests should succeed");

    let (sf_rsp, uni_rsp) = tokio::join! { sf_conn.rx().recv(), uni_conn.rx().recv() };
    assert_eq!(
        sf_rsp,
        Some(HelloWorldResponse {
            world: "san francisco".to_string()
        })
    );
    assert_eq!(
        uni_rsp,
        Some(HelloWorldResponse {
            world: "universe".to_string()
        })
    );
}
