use mgnp_pitch::{
    message::{InboundMessage, Nak, OutboundMessage},
    registry,
    tricky_pipe::bidi::{BiDi, SerBiDi},
    Frame, Registry, Wire,
};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, RwLock},
};
use tokio::sync::{mpsc, oneshot};
pub use tracing::Instrument;
use uuid::{uuid, Uuid};

pub const HELLO_WORLD_UUID: Uuid = uuid!("6c5361c3-cb70-4651-9c6e-8dd3e3625910");

pub fn hello_world_id() -> registry::Identity {
    registry::Identity {
        id: HELLO_WORLD_UUID,
        kind: registry::IdentityKind::Name("helloworld".into()),
    }
}

pub fn trace_init() {
    let filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive(tracing::Level::TRACE.into())
        .from_env_lossy();
    let _ = tracing_subscriber::fmt()
        .with_file(true)
        .with_line_number(true)
        .with_target(false)
        .with_test_writer()
        .with_env_filter(filter)
        .without_time()
        .try_init();
}

pub fn make_bidis<In, Out>(cap: u8) -> (SerBiDi, BiDi<In, Out>)
where
    In: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    Out: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    let (in_tx, in_rx) = {
        let pipe = tricky_pipe::TrickyPipe::new(cap);
        let rx = pipe.receiver().unwrap();
        let tx = pipe.deser_sender();
        (tx, rx)
    };
    let (out_tx, out_rx) = {
        let pipe = tricky_pipe::TrickyPipe::new(cap);
        let rx = pipe.ser_receiver().unwrap();
        let tx = pipe.sender();
        (tx, rx)
    };
    let bidi_in = SerBiDi::from_pair(in_tx, out_rx);
    let bidi_out = BiDi::from_pair(out_tx, in_rx);
    (bidi_in, bidi_out)
}

#[derive(Default, Clone)]
pub struct TestRegistry {
    svcs: Arc<RwLock<HashMap<registry::Identity, mpsc::Sender<InboundConnect>>>>,
}

pub struct TestWire {
    tx: mpsc::Sender<Vec<u8>>,
    rx: mpsc::Receiver<Vec<u8>>,
}

#[derive(Debug)]
pub struct TestFrame(Vec<u8>);

pub struct InboundConnect {
    pub hello: Vec<u8>,
    pub rsp: oneshot::Sender<Result<SerBiDi, Nak>>,
}

#[derive(Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HelloWorldRequest {
    pub hello: String,
}

#[derive(Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HelloWorldResponse {
    pub world: String,
}

// === impl TestRegistry ===

impl Registry for TestRegistry {
    async fn connect(&self, identity: registry::Identity, hello: &[u8]) -> Result<SerBiDi, Nak> {
        let Some(svc) = self.svcs.read().unwrap().get(&identity).cloned() else {
            return Err(Nak::NotFound);
        };

        let (rsp, rx) = oneshot::channel();
        if svc
            .send(InboundConnect {
                hello: hello.to_vec(),
                rsp,
            })
            .await
            .is_err()
        {
            // receiver dropped, svc is dead.
            self.svcs.write().unwrap().remove(&identity);
            return Err(Nak::NotFound);
        }

        rx.await.map_err(|_| Nak::NotFound)?
    }
}

impl TestRegistry {
    pub fn add_service(&self, identity: registry::Identity) -> mpsc::Receiver<InboundConnect> {
        let (tx, rx) = mpsc::channel(1);
        self.svcs.write().unwrap().insert(identity, tx);
        rx
    }

    pub fn spawn_hello_world(&self) {
        let mut chan = self.add_service(hello_world_id());
        tokio::spawn(
            async move {
                let mut worker = 1;
                while let Some(req) = chan.recv().await {
                    let InboundConnect { hello, rsp } = req;
                    tracing::info!(?hello, "hello world service received connection");
                    let (their_chan, my_chan) =
                        make_bidis::<HelloWorldRequest, HelloWorldResponse>(8);
                    tokio::spawn(
                        async move {
                            tracing::debug!("hello world worker running...");
                            while let Some(req) = my_chan.rx().recv().await {
                                tracing::info!(?req);
                                assert_eq!(req.hello, "hello");
                                my_chan
                                    .tx()
                                    .send(HelloWorldResponse {
                                        world: "world".into(),
                                    })
                                    .await
                                    .unwrap();
                            }
                        }
                        .instrument(tracing::info_span!("hello_world_worker", worker)),
                    );
                    worker += 1;
                    let sent = rsp.send(Ok(their_chan)).is_ok();
                    tracing::debug!(?sent);
                }
            }
            .instrument(tracing::info_span!("hello_world_service")),
        );
    }
}

// === impl TestWire ===

impl TestWire {
    pub fn new() -> (Self, Self) {
        let (tx1, rx1) = mpsc::channel(8);
        let (tx2, rx2) = mpsc::channel(8);
        (Self { tx: tx1, rx: rx2 }, Self { tx: tx2, rx: rx1 })
    }
}

impl Wire for TestWire {
    type Frame = TestFrame;
    type Error = &'static str;

    async fn recv(&mut self) -> Result<Self::Frame, &'static str> {
        let frame = self.rx.recv().await;
        tracing::info!(frame = ?frame.as_ref().map(HexSlice::new), "RECV");
        frame
            .ok_or("the send end of this wire has been dropped")
            .map(TestFrame)
    }

    async fn send(&mut self, msg: OutboundMessage<'_>) -> Result<(), &'static str> {
        // TODO(eliza): this is awkward, the trickypipe SerRecvRef API needs to
        // suck less so we don't need to do it like this...
        tracing::info!(?msg, "sending message");
        let frame = match msg {
            OutboundMessage::Control(ctrl) => {
                postcard::to_allocvec(&InboundMessage::Control(ctrl)).unwrap()
            }
            OutboundMessage::Data {
                local_id,
                remote_id,
                data,
            } => {
                let data = data.to_vec().unwrap();
                postcard::to_allocvec(&InboundMessage::Data {
                    local_id,
                    remote_id,
                    data: &data[..],
                })
                .unwrap()
            }
        };

        tracing::info!(frame = ?HexSlice::new(&frame), "SEND");
        self.tx
            .send(frame)
            .await
            .map_err(|_| "the recv end of this wire has been dropped")
    }
}

// === impl TestFrame ===

impl Frame for TestFrame {
    fn as_bytes(&self) -> &[u8] {
        self.0.as_ref()
    }
}

struct HexSlice<'a>(&'a [u8]);

impl<'a> HexSlice<'a> {
    fn new<T>(data: &'a T) -> HexSlice<'a>
    where
        T: ?Sized + AsRef<[u8]> + 'a,
    {
        HexSlice(data.as_ref())
    }
}

impl fmt::Debug for HexSlice<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[")?;

        let mut bytes = self.0.iter();
        if let Some(byte) = bytes.next() {
            write!(f, "{byte:02x}")?;
            for byte in bytes {
                write!(f, " {byte:02x}")?;
            }
        }

        f.write_str("]")
    }
}
