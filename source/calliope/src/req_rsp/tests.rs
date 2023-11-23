use super::*;
use crate::tests::*;
use std::sync::Arc;

#[tokio::test]
async fn req_rsp() {
    let remote_registry: TestRegistry = TestRegistry::default();
    let conns = remote_registry.add_service(svcs::EchoDelayService::identity());
    tokio::spawn(svcs::EchoDelayService::serve(conns));

    let fixture = Fixture::new()
        .spawn_local(Default::default())
        .spawn_remote(remote_registry);

    let mut connector = fixture.local_iface().connector::<svcs::EchoDelayService>();

    let chan = connect(&mut connector, "echo-delay", ()).await;
    let client = Arc::new(Client::<svcs::EchoDelayService>::new(chan));

    let dispatcher = tokio::spawn({
        let client = client.clone();
        async move {
            client.dispatch().await.expect("dispatcher died");
        }
    });

    let rsp_futs = (10..100).rev().map(|val| {
        let client = client.clone();
        tokio::spawn(
            async move {
                tracing::info!(val, "sending request...");
                let rsp = client.request(svcs::Echo { val }).await;
                tracing::info!(?rsp, "recieved response");
                assert_eq!(rsp, Ok(svcs::Echo { val }));
            }
            .instrument(tracing::info_span!("request", val)),
        )
    });

    for fut in rsp_futs {
        fut.await.expect("response task should not have panicked!");
    }

    client.shutdown();

    dispatcher
        .await
        .expect("dispatcher task should not have panicked");

    fixture.finish_test().await
}
