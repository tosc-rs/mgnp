use super::*;
use crate::{message::Rejection, service::Identity};
use svcs::{HelloWorldRequest, HelloWorldResponse};

#[tokio::test]
async fn basically_works() {
    let remote_registry: TestRegistry = TestRegistry::default();
    remote_registry.spawn_hello_world();

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry);

    let mut connector = fixture.local_iface().connector::<svcs::HelloWorld>();

    let chan = connect(&mut connector, "hello-world", ()).await;
    chan.tx()
        .send(svcs::hello_req("hello"))
        .await
        .expect("send request");
    let rsp = chan.rx().recv().await;
    assert_eq!(
        rsp,
        Ok(HelloWorldResponse {
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

    let mut connector = fixture.local_iface().connector::<svcs::HelloWithHello>();

    let chan = connect(
        &mut connector,
        "hello-hello",
        svcs::HelloHello {
            hello: "hello".into(),
        },
    )
    .await;

    chan.tx()
        .send(svcs::hello_req("hello"))
        .await
        .expect("send request");
    let rsp = chan.rx().recv().await;
    assert_eq!(
        rsp,
        Ok(HelloWorldResponse {
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

    let mut connector = fixture.local_iface().connector::<svcs::HelloWithHello>();

    // establish a good connection with a valid hello
    let chan = connect(
        &mut connector,
        "hello-hello",
        svcs::HelloHello {
            hello: "hello".into(),
        },
    )
    .await;

    // now try to connect again with an invalid hello
    connect_should_nak(
        &mut connector,
        "hello-hello",
        svcs::HelloHello {
            hello: "goodbye".into(),
        },
        Rejection::ServiceRejected,
    )
    .await;

    // the good connection should stil lwork
    chan.tx()
        .send(svcs::hello_req("hello"))
        .await
        .expect("send request");
    let rsp = chan.rx().recv().await;
    assert_eq!(
        rsp,
        Ok(HelloWorldResponse {
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

    let mut connector = fixture.local_iface().connector::<svcs::HelloWorld>();

    let chan1 = connect(&mut connector, "hello-world", ()).await;

    let chan2 = connect(&mut connector, "hello-world", ()).await;

    tokio::try_join! {
        chan1.tx().send(svcs::hello_req("hello")),
        chan2.tx().send(svcs::hello_req("hello"))
    }
    .expect("send should work");

    let (rsp1, rsp2) = tokio::join! {
        chan1.rx().recv(),
        chan2.rx().recv(),
    };

    assert_eq!(
        rsp1,
        Ok(HelloWorldResponse {
            world: "world".to_string()
        })
    );
    assert_eq!(
        rsp2,
        Ok(HelloWorldResponse {
            world: "world".to_string()
        })
    );

    fixture.finish_test().await;
}

/// Tests routing to services of different types (UUIDs).
#[tokio::test]
async fn service_type_routing() {
    // remote comes up with NO services present...
    let remote_registry: TestRegistry = TestRegistry::default();

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry.clone());

    let iface = fixture.local_iface();

    // create connectors for both services
    let mut helloworld_connector = iface.connector::<svcs::HelloWorld>();
    let mut hellohello_connector = iface.connector::<svcs::HelloWithHello>();

    // attempts to initiate connections should fail when the remote services
    // don't exist
    connect_should_nak(
        &mut helloworld_connector,
        "hello-world",
        (),
        Rejection::NotFound,
    )
    .await;
    connect_should_nak(
        &mut hellohello_connector,
        "hello-hello",
        svcs::HelloHello {
            hello: "hello".into(),
        },
        Rejection::NotFound,
    )
    .await;

    // add a service
    remote_registry.spawn_hello_world();

    // connecting to HelloHello should still fail
    connect_should_nak(
        &mut hellohello_connector,
        "hello-hello",
        svcs::HelloHello {
            hello: "hello".into(),
        },
        Rejection::NotFound,
    )
    .await;

    // ... but connecting to HelloWorld should succeed
    let helloworld_chan = connect(&mut helloworld_connector, "hello-world", ()).await;

    helloworld_chan
        .tx()
        .send(svcs::hello_req("hello"))
        .await
        .expect("send request");
    let rsp = helloworld_chan.rx().recv().await;
    assert_eq!(
        rsp,
        Ok(HelloWorldResponse {
            world: "world".to_string()
        })
    );

    // add the other service
    remote_registry.spawn_hello_with_hello();

    let hellohello_chan = connect(
        &mut hellohello_connector,
        "hello-hello",
        svcs::HelloHello {
            hello: "hello".into(),
        },
    )
    .await;

    hellohello_chan
        .tx()
        .send(svcs::hello_req("hello"))
        .await
        .expect("send request");

    helloworld_chan
        .tx()
        .send(svcs::hello_req("hello"))
        .await
        .expect("send request");

    let rsp = helloworld_chan.rx().recv().await;
    assert_eq!(
        rsp,
        Ok(HelloWorldResponse {
            world: "world".to_string()
        })
    );

    let rsp = hellohello_chan.rx().recv().await;
    assert_eq!(
        rsp,
        Ok(HelloWorldResponse {
            world: "world".to_string()
        })
    );
}

/// Tests routing to multiple service instances of the same service *interface*,
/// based on their identity.
#[tokio::test]
async fn service_identity_routing() {
    // remote comes up with NO services present...
    let remote_registry: TestRegistry = TestRegistry::default();

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry.clone());

    let mut connector = fixture.local_iface().connector::<svcs::HelloWorld>();

    // attempts to initiate connections should fail when the remote services
    // don't exist
    connect_should_nak(&mut connector, "hello-world", (), Rejection::NotFound).await;
    connect_should_nak(&mut connector, "hello-sf", (), Rejection::NotFound).await;
    connect_should_nak(&mut connector, "hello-universe", (), Rejection::NotFound).await;

    // add the 'hello-sf' service
    let conns = remote_registry.add_service(Identity::from_name::<svcs::HelloWorld>("hello-sf"));
    tokio::spawn(svcs::serve_hello("hello sf", "san francisco", conns));

    // connecting to hello-world and hello-universe should still fail...
    connect_should_nak(&mut connector, "hello-world", (), Rejection::NotFound).await;
    connect_should_nak(&mut connector, "hello-universe", (), Rejection::NotFound).await;

    // ... but connecting to 'hello-sf' should succeed
    let sf_conn = connect(&mut connector, "hello-sf", ()).await;

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
        Ok(HelloWorldResponse {
            world: "san francisco".to_string()
        })
    );

    // add the 'hello-universe' service
    let conns =
        remote_registry.add_service(Identity::from_name::<svcs::HelloWorld>("hello-universe"));
    tokio::spawn(svcs::serve_hello("hello universe", "universe", conns));

    // connecting to hello-world should still fail
    connect_should_nak(&mut connector, "hello-world", (), Rejection::NotFound).await;

    // ... but connecting to 'hello-sf' should succeed
    let uni_conn = connect(&mut connector, "hello-universe", ()).await;

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
        Ok(HelloWorldResponse {
            world: "san francisco".to_string()
        })
    );
    assert_eq!(
        uni_rsp,
        Ok(HelloWorldResponse {
            world: "universe".to_string()
        })
    );
}

#[tokio::test]
async fn reset_closed() {
    let remote_registry: TestRegistry = TestRegistry::default();
    let conns = remote_registry.add_service(svcs::hello_world_id());
    let shutdown = Arc::new(tokio::sync::Notify::new());

    tokio::spawn(svcs::serve_hello_with_shutdown(
        "hello",
        "world",
        conns,
        shutdown.clone(),
    ));

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry);

    let mut connector = fixture.local_iface().connector::<svcs::HelloWorld>();

    let chan1 = connect(&mut connector, "hello-world", ()).await;

    let chan2 = connect(&mut connector, "hello-world", ()).await;

    tokio::try_join! {
        chan1.tx().send(svcs::hello_req("hello")),
        chan2.tx().send(svcs::hello_req("hello"))
    }
    .expect("send should work");

    let (rsp1, rsp2) = tokio::join! {
        chan1.rx().recv(),
        chan2.rx().recv(),
    };

    assert_eq!(
        rsp1,
        Ok(HelloWorldResponse {
            world: "world".to_string()
        })
    );
    assert_eq!(
        rsp2,
        Ok(HelloWorldResponse {
            world: "world".to_string()
        })
    );

    // now shut down the remote service
    tracing::info!("");
    tracing::info!("!!! shutting down remote service !!!");
    tracing::info!("");
    shutdown.notify_waiters();

    let send2 = tokio::join! {
        chan1.tx().send(svcs::hello_req("hello")),
        chan2.tx().send(svcs::hello_req("hello"))
    };

    let _ = dbg!(send2);

    let (rsp1, rsp2) = tokio::join! {
        chan1.rx().recv(),
        chan2.rx().recv(),
    };

    assert_eq!(
        dbg!(rsp1),
        Err(tricky_pipe::mpsc::error::RecvError::Error(
            Reset::BecauseISaidSo
        ))
    );

    assert_eq!(
        dbg!(rsp2),
        Err(tricky_pipe::mpsc::error::RecvError::Error(
            Reset::BecauseISaidSo
        ))
    );

    fixture.finish_test().await;
}
