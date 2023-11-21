use super::*;
use crate::{
    message::{self, InboundFrame, OutboundFrame},
    Wire,
};
use tricky_pipe::serbox;

#[tokio::test]
async fn reset_decode_error() {
    let remote_registry: TestRegistry = TestRegistry::default();
    remote_registry.spawn_hello_world();

    let mut fixture = Fixture::new().spawn_remote(remote_registry);
    let mut wire = fixture.take_local_wire();
    let mut hellobox = serbox::Sharer::new();
    let hello = hellobox.share(()).await;

    wire.send(OutboundFrame::connect(
        crate::Id::new(1),
        svcs::hello_world_id(),
        hello,
    ))
    .await
    .unwrap();

    let frame = wire.recv().await.unwrap();
    let msg = InboundFrame::from_bytes(&frame[..]);
    assert_eq!(
        msg,
        Ok(InboundFrame {
            header: message::Header::Ack {
                local_id: crate::Id::new(1),
                remote_id: crate::Id::new(1),
            },
            body: &[]
        })
    );

    let mut out_frame = postcard::to_allocvec(&message::Header::Data {
        local_id: crate::Id::new(1),
        remote_id: crate::Id::new(1),
    })
    .unwrap();
    out_frame.extend(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);

    wire.send_bytes(out_frame).await.unwrap();

    let frame = wire.recv().await.unwrap();
    let msg = InboundFrame::from_bytes(&frame[..]);
    assert_eq!(
        msg,
        Ok(InboundFrame {
            header: message::Header::Reset {
                remote_id: crate::Id::new(1),
                reason: message::Reset::YouDoneGoofed(message::DecodeError::Body(
                    message::DecodeErrorKind::UnexpectedEnd
                ))
            },
            body: &[]
        })
    );
}

#[tokio::test]
async fn reset_no_such_conn() {
    let remote_registry: TestRegistry = TestRegistry::default();
    remote_registry.spawn_hello_world();

    let mut fixture = Fixture::new().spawn_remote(remote_registry);
    let mut wire = fixture.take_local_wire();
    let mut hellobox = serbox::Sharer::new();
    let hello = hellobox.share(()).await;

    wire.send(OutboundFrame::connect(
        crate::Id::new(1),
        svcs::hello_world_id(),
        hello,
    ))
    .await
    .unwrap();

    let frame = wire.recv().await.unwrap();
    let msg = InboundFrame::from_bytes(&frame[..]);
    assert_eq!(
        msg,
        Ok(InboundFrame {
            header: message::Header::Ack {
                local_id: crate::Id::new(1),
                remote_id: crate::Id::new(1),
            },
            body: &[]
        })
    );

    let chan = tricky_pipe::mpsc::TrickyPipe::new(8);
    let rx = chan.ser_receiver().unwrap();
    let tx = chan.sender();
    tx.try_send(svcs::HelloWorldRequest {
        hello: "hello".into(),
    })
    .unwrap();

    let body = rx.try_recv().unwrap();

    let out_frame = {
        let frame = OutboundFrame {
            header: message::Header::Data {
                local_id: crate::Id::new(1),
                remote_id: crate::Id::new(1), // good conn ID
            },
            body: message::OutboundData::Data(body),
        };
        frame.to_vec().unwrap()
    };

    wire.send_bytes(out_frame).await.unwrap();

    let frame = wire.recv().await.unwrap();
    let msg = InboundFrame::from_bytes(&frame[..]).unwrap();
    assert_eq!(
        postcard::from_bytes(msg.body),
        Ok(svcs::HelloWorldResponse {
            world: "world".into()
        })
    );

    // another message, with a bad conn ID
    tx.try_send(svcs::HelloWorldRequest {
        hello: "hello".into(),
    })
    .unwrap();
    let body = rx.try_recv().unwrap();
    let out_frame = OutboundFrame {
        header: message::Header::Data {
            remote_id: crate::Id::new(666), // bad conn ID
            local_id: crate::Id::new(1),
        },
        body: message::OutboundData::Data(body),
    }
    .to_vec()
    .unwrap();

    wire.send_bytes(out_frame).await.unwrap();
    let frame = wire.recv().await.unwrap();
    let msg = dbg!(InboundFrame::from_bytes(&frame[..]));
    assert_eq!(
        msg,
        Ok(InboundFrame {
            header: message::Header::Reset {
                remote_id: crate::Id::new(1),
                reason: message::Reset::NoSuchConn,
            },
            body: &[]
        })
    );

    // another message, with a differently conn ID
    tx.try_send(svcs::HelloWorldRequest {
        hello: "hello".into(),
    })
    .unwrap();
    let body = rx.try_recv().unwrap();
    let out_frame = OutboundFrame {
        header: message::Header::Data {
            remote_id: crate::Id::new(1),
            local_id: crate::Id::new(666), // bad conn ID
        },
        body: message::OutboundData::Data(body),
    }
    .to_vec()
    .unwrap();

    wire.send_bytes(out_frame).await.unwrap();
    let frame = wire.recv().await.unwrap();
    let msg = dbg!(InboundFrame::from_bytes(&frame[..]));
    assert_eq!(
        msg,
        Ok(InboundFrame {
            header: message::Header::Reset {
                remote_id: crate::Id::new(666),
                reason: message::Reset::NoSuchConn,
            },
            body: &[]
        })
    );
}
