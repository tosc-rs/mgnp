use super::*;
use crate::{
    message::{self, DecodeError, DecodeErrorKind, Header, InboundFrame, OutboundFrame, Reset},
    Id, Wire,
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
        Id::new(1),
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
            header: Header::Ack {
                local_id: Id::new(1),
                remote_id: Id::new(1),
            },
            body: &[]
        })
    );

    let mut out_frame = postcard::to_allocvec(&Header::Data {
        local_id: Id::new(1),
        remote_id: Id::new(1),
    })
    .unwrap();
    out_frame.extend(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);

    wire.send_bytes(out_frame).await.unwrap();

    let frame = wire.recv().await.unwrap();
    expect_inbound_frame(
        frame,
        InboundFrame {
            header: Header::Reset {
                remote_id: Id::new(1),
                reason: Reset::YouDoneGoofed(DecodeError::Body(DecodeErrorKind::UnexpectedEnd)),
            },
            body: &[],
        },
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
        Id::new(1),
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
            header: Header::Ack {
                local_id: Id::new(1),
                remote_id: Id::new(1),
            },
            body: &[]
        })
    );

    let chan = tricky_pipe::mpsc::TrickyPipe::new(8);
    let rx = chan.ser_receiver().unwrap();
    let tx = chan.sender();

    let data_frame = |header: Header| {
        tx.try_send(svcs::HelloWorldRequest {
            hello: "hello".into(),
        })
        .expect("send should just work");
        let body = rx.try_recv().expect("recv should just work");
        let frame = OutboundFrame {
            header,
            body: message::OutboundData::Data(body),
        };
        tracing::info!(frame = %format_args!("{frame:#?}"), "OUTBOUND FRAME");
        frame.to_vec().expect("frame must serialize")
    };

    wire.send_bytes(data_frame(Header::Data {
        local_id: Id::new(1), // known good ID
        remote_id: Id::new(1),
    }))
    .await
    .unwrap();

    let frame = wire.recv().await.unwrap();
    let msg = InboundFrame::from_bytes(&frame[..]).unwrap();
    assert_eq!(
        postcard::from_bytes(msg.body),
        Ok(svcs::HelloWorldResponse {
            world: "world".into()
        })
    );

    // another message, with a bad conn ID
    wire.send_bytes(data_frame(Header::Data {
        local_id: Id::new(1),
        remote_id: Id::new(666), // bad conn ID
    }))
    .await
    .unwrap();

    let frame = wire.recv().await.unwrap();
    expect_inbound_frame(
        frame,
        InboundFrame {
            header: Header::Reset {
                remote_id: Id::new(1),
                reason: Reset::NoSuchConn,
            },
            body: &[],
        },
    );

    // another message, with a differently conn ID
    wire.send_bytes(data_frame(Header::Data {
        local_id: Id::new(666), // bad conn ID
        remote_id: Id::new(1),
    }))
    .await
    .unwrap();

    let frame = wire.recv().await.unwrap();
    expect_inbound_frame(
        frame,
        InboundFrame {
            header: Header::Reset {
                remote_id: Id::new(666),
                reason: Reset::NoSuchConn,
            },
            body: &[],
        },
    );
}

#[track_caller]
fn expect_inbound_frame(frame: impl AsRef<[u8]>, expected: InboundFrame<'_>) {
    let decoded = InboundFrame::from_bytes(frame.as_ref());
    tracing::info!(frame = %format_args!("{decoded:#?}"), "INBOUND FRAME");
    assert_eq!(decoded, Ok(expected));
}
