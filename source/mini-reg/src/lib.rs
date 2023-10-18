#![cfg_attr(not(test), no_std)]

// The goal is to have something like this:

use core::marker::PhantomData;

use serde::{Serialize, de::DeserializeOwned};
use tricky_pipe::{StaticTrickyPipe, Sender, Receiver, DeserSender, SerReceiver, SerPermit};
use uuid::{uuid, Uuid};

pub trait ServiceSocket {
    const UUID: Uuid;

    /// Client Hello Message
    type ClientHello: 'static;
    /// Client normal send message
    type ClientSent: 'static;

    /// Service normal send message
    type ServiceSent: 'static;
    /// Service "hello rejected" message
    type ServiceConnectError: 'static;
}

pub enum ConnectResponse<SerConErr> {
    Accepted,
    Rejected(SerConErr),
}

struct FakeServiceOne;
struct FakeServiceTwo;

impl ServiceSocket for FakeServiceOne {
    const UUID: Uuid = uuid!("779FB751-6E2B-47A4-B067-7983D60A96BC");
    type ClientHello = u8;
    type ClientSent = u16;
    type ServiceSent = u32;
    type ServiceConnectError = u64;
}

impl ServiceSocket for FakeServiceTwo {
    const UUID: Uuid = uuid!("C03EFF8E-DB39-43CB-9DD1-10077A8B47A9");
    type ClientHello = i8;
    type ClientSent = i16;
    type ServiceSent = i32;
    type ServiceConnectError = i64;
}

macro_rules! table {
    ($($bob:tt)*) => {};
}

table! {
    one: FakeServiceOne => {
        max_conns: 1,
        client_msg_depth: 4,
        service_msg_depth: 2,
    },
    two: FakeServiceTwo => {
        max_conns: 1,
        client_msg_depth: 2,
        service_msg_depth: 1,
    },
}

enum ErasedConnectError {
    ServiceNotFound,
    ServiceClosed,
    OtherInternal,
    ServiceError,
}

impl<T: ServiceSocket> From<ConnectError<T>> for ErasedConnectError {
    fn from(value: ConnectError<T>) -> Self {
        match value {
            ConnectError::ServiceNotFound => ErasedConnectError::ServiceNotFound,
            ConnectError::ServiceClosed => ErasedConnectError::ServiceClosed,
            ConnectError::OtherInternal => ErasedConnectError::OtherInternal,
            ConnectError::ServiceError(_) => ErasedConnectError::ServiceError,
        }
    }
}

enum ConnectError<Svc: ServiceSocket> {
    ServiceNotFound,
    ServiceClosed,
    OtherInternal,
    ServiceError(Svc::ServiceConnectError),
}

// The macro should produce this:

struct BidiClient<Svc: ServiceSocket> {
    tx: Sender<Svc::ClientSent>,
    rx: Receiver<Svc::ServiceSent>,
}
struct ErasedBidiClient {
    tx: DeserSender,
    rx: SerReceiver,
}
struct OneShot<T>(PhantomData<T>);
struct OneShotSender<T>(PhantomData<T>);
struct OneShotBuf {
    // TODO: This needs to hold some kind of borrowed slice
    // using intrusive bullshit?
}
struct HelloReply<Svc: ServiceSocket> {
    hello: Svc::ClientHello,
    reply: OneShotSender<Result<BidiClient<Svc>, Svc::ServiceConnectError>>,
}

static FakeServiceOneChannelIn: StaticTrickyPipe<
    <FakeServiceOne as ServiceSocket>::ClientSent,
    4,
> = StaticTrickyPipe::new();

static FakeServiceOneChannelOut: StaticTrickyPipe<
    <FakeServiceOne as ServiceSocket>::ServiceSent,
    2,
> = StaticTrickyPipe::new();

static FakeServiceOneChannelConn: StaticTrickyPipe<
    HelloReply<FakeServiceOne>,
    2, // TODO
> = StaticTrickyPipe::new();

//
// THIS CODE assumes that the service provides the buffers. I'm not sure this
// is correct.
//

// Typed:
//
// type ConnResult<Svc: ServiceSocket> = Result<BidiClient<Svc>, ConnectError<Svc>>
//
// // * Looks up "ChannelConn" in the registry
// // * Creates a OneShot for the connection reply
// // * Sends the HelloReply to the service
// // * Awaits response
// // * Either gets an error, or now has a typed client channel
// // * BidiClient holds a (Sender<ClientSent>, Receiver<ServiceSent>)
//
// let conn: ConnResult<FakeServiceOne> = table
//     .connect::<FakeServiceOne>(
//         FakeServiceOne::ClientHello,
//     ).await;
//
// let conn: BidiClient<Svc> = conn?;

// Untyped:
//
// // TODO: How to get type-erased error? We could make a whole ass erased tricky pipe
// // instead of a oneshot, but that seems wasteful...
// //
// type ErasedConnResult = Result<ErasedBidiClient, ???>;
//
// // * Looks up "ChannelConn" by UUID
// // * UH OH, we can't send anything async except with DeserSender, but then
// //   how do we send along the socket channel?
//
// let conn: ErasedConnResult = table
//     .connect_by_uuid(
//         request.uuid,
//         request.hello_bytes,
//     )
//     .await;
//
// let conn: ErasedBidiClient = conn?;


fn do_send<Svc: ServiceSocket>(
    bytes: &[u8],
    // TODO: This needs to be erased somehow
    reply: OneShotSender<Result<BidiClient<Svc>, Svc::ServiceConnectError>>,
    erase_res: SerPermit<'_>,
) -> Result<(), ()>
where
    // TODO: The variance here is fucky
    Svc::ClientHello: DeserializeOwned,
    Svc: 'static,
{
    let f = || {
        let hello: Svc::ClientHello = postcard::from_bytes(bytes).ok()?;
        Some(HelloReply {
            hello,
            reply,
        })
    };
    erase_res.send_with(f)
}








