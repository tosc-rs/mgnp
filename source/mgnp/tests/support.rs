#![cfg(feature = "alloc")]
use mgnp::{
    message::{Nak, OutboundFrame},
    registry,
    tricky_pipe::{
        bidi::{BiDi, SerBiDi},
        mpsc::TrickyPipe,
    },
    Interface, Registry, Wire,
};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, RwLock},
};
use tokio::sync::{mpsc, oneshot, Notify};
pub use tracing::Instrument;
use uuid::{uuid, Uuid};

pub const HELLO_WORLD_UUID: Uuid = uuid!("6c5361c3-cb70-4651-9c6e-8dd3e3625910");

pub fn hello_world_id() -> registry::Identity {
    registry::Identity::from_name::<HelloWorldService>("hello-world")
}

pub struct HelloWorldService;

impl registry::Service for HelloWorldService {
    type ClientMsg = HelloWorldRequest;
    type ServerMsg = HelloWorldResponse;
    type ConnectError = ();
    type Hello = ();
    const UUID: Uuid = HELLO_WORLD_UUID;
}

pub const HELLO_WITH_HELLO_UUID: Uuid = uuid!("9442b293-93d8-48b9-bbf7-52f636462bfe");

pub fn hello_with_hello_id() -> registry::Identity {
    registry::Identity::from_name::<HelloWithHelloService>("hello-hello")
}

pub struct HelloWithHelloService;

pub struct Fixture<L, R> {
    local: L,
    remote: R,
    test_done: Arc<Notify>,
}

impl Fixture<TestWire, TestWire> {
    pub fn new() -> Self {
        trace_init();
        let (local, remote) = TestWire::new();
        Self {
            local,
            remote,
            test_done: Arc::new(Notify::new()),
        }
    }
}

type Running = (Interface, tokio::task::JoinHandle<()>);

impl<R> Fixture<TestWire, R> {
    pub fn spawn_local(self, registry: TestRegistry) -> Fixture<Running, R> {
        let Fixture {
            local,
            remote,
            test_done,
        } = self;

        let (iface, machine) = Interface::new::<_, _, { mgnp::DEFAULT_MAX_CONNS }>(
            local,
            registry,
            TrickyPipe::new(8),
        );
        Fixture {
            local: (
                iface,
                tokio::spawn(interface("local", machine, test_done.clone())),
            ),
            remote,
            test_done,
        }
    }
}

impl<L> Fixture<L, TestWire> {
    pub fn spawn_remote(self, registry: TestRegistry) -> Fixture<L, Running> {
        let Fixture {
            local,
            remote,
            test_done,
        } = self;

        let (iface, machine) = Interface::new::<_, _, { mgnp::DEFAULT_MAX_CONNS }>(
            remote,
            registry,
            TrickyPipe::new(8),
        );
        Fixture {
            local,
            remote: (
                iface,
                tokio::spawn(interface("remote", machine, test_done.clone())),
            ),
            test_done,
        }
    }
}

impl<L> Fixture<L, Running> {
    pub fn remote_iface(&self) -> Interface {
        self.remote.0.clone()
    }
}

impl<R> Fixture<Running, R> {
    pub fn local_iface(&self) -> Interface {
        self.local.0.clone()
    }
}

impl Fixture<Running, Running> {
    pub async fn finish_test(self) {
        tracing::info!("shutting down test...");
        // complete the test and wait for both the "local" and "remote" interface
        // tasks to complete, so we can ensure they didn't panic.
        self.test_done.notify_waiters();
        self.local
            .1
            .await
            .expect("local interface task should not panic");
        self.remote
            .1
            .await
            .expect("remote interface task should not panic");
    }
}

#[tracing::instrument(level = tracing::Level::INFO, skip(machine, test_done))]
async fn interface(
    peer: &'static str,
    mut machine: mgnp::Machine<TestWire, TestRegistry, { mgnp::DEFAULT_MAX_CONNS }>,
    test_done: Arc<Notify>,
) {
    tokio::select! {
        res = machine.run() => {
            tracing::info!(?res, "local interface run loop terminated!");
            // the remote may be terminated by an interface error, since
            // the wire will be dropped.
        },
        _ = test_done.notified() => {
            tracing::debug!("test done, local shutting down...");
        },
    }
}

impl registry::Service for HelloWithHelloService {
    type ClientMsg = HelloWorldRequest;
    type ServerMsg = HelloWorldResponse;
    type ConnectError = ();
    type Hello = HelloHello;
    const UUID: Uuid = HELLO_WORLD_UUID;
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
        let pipe = TrickyPipe::new(cap);
        let rx = pipe.receiver().unwrap();
        let tx = pipe.deser_sender();
        (tx, rx)
    };
    let (out_tx, out_rx) = {
        let pipe = TrickyPipe::new(cap);
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
pub struct HelloHello {
    pub hello: String,
}

#[derive(Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HelloWorldResponse {
    pub world: String,
}

// === impl TestRegistry ===

impl Registry for TestRegistry {
    #[tracing::instrument(level = tracing::Level::INFO, name = "Registry::connect", skip(self, hello),)]
    async fn connect(&self, identity: registry::Identity, hello: &[u8]) -> Result<SerBiDi, Nak> {
        let Some(svc) = self.svcs.read().unwrap().get(&identity).cloned() else {
            tracing::info!("REGISTRY: service not found!");
            return Err(Nak::NotFound);
        };

        tracing::info!("REGISTRY: service found");

        let (rsp, rx) = oneshot::channel();
        if svc
            .send(InboundConnect {
                hello: hello.to_vec(),
                rsp,
            })
            .await
            .is_err()
        {
            tracing::info!("REGISTRY: service dead!");
            // receiver dropped, svc is dead.
            self.svcs.write().unwrap().remove(&identity);
            return Err(Nak::NotFound);
        }

        rx.await.map_err(|_| Nak::NotFound)?
    }
}

impl TestRegistry {
    #[tracing::instrument(level = tracing::Level::INFO, name = "Registry::add_service", skip(self),)]
    pub fn add_service(&self, identity: registry::Identity) -> mpsc::Receiver<InboundConnect> {
        let (tx, rx) = mpsc::channel(1);
        self.svcs.write().unwrap().insert(identity, tx);
        tracing::info!("REGISTRY: service added");
        rx
    }

    pub fn spawn_hello_world(&self) -> &Self {
        let mut chan = self.add_service(hello_world_id());
        tokio::spawn(
            async move {
                let mut worker = 1;
                while let Some(req) = chan.recv().await {
                    let InboundConnect { hello, rsp } = req;
                    tracing::info!(?hello, "hello world service received connection");
                    let (their_chan, my_chan) =
                        make_bidis::<HelloWorldRequest, HelloWorldResponse>(8);
                    tokio::spawn(hello_worker(worker, my_chan));
                    worker += 1;
                    let sent = rsp.send(Ok(their_chan)).is_ok();
                    tracing::debug!(?sent);
                }
            }
            .instrument(tracing::info_span!("hello_world_service")),
        );
        self
    }

    pub fn spawn_hello_with_hello(&self) -> &Self {
        let mut chan = self.add_service(hello_with_hello_id());
        tokio::spawn(
            async move {
                let mut worker = 1;
                while let Some(req) = chan.recv().await {
                    let InboundConnect { hello, rsp } = req;
                    tracing::info!(?hello, "hellohello service received connection");
                    let hello = postcard::from_bytes::<HelloHello>(&hello)
                        .expect("hellohello message must deserialize!");
                    let res = if hello.hello == "hello" {
                        tracing::info!(?hello, "hellohello service received hello");
                        let (their_chan, my_chan) =
                            make_bidis::<HelloWorldRequest, HelloWorldResponse>(8);
                        tokio::spawn(hello_worker(worker, my_chan));
                        worker += 1;
                        Ok(their_chan)
                    } else {
                        tracing::info!(
                            ?hello,
                            "hellohello service received valid non-matching hello!"
                        );
                        Err(Nak::Rejected)
                    };
                    let sent = rsp.send(res).is_ok();
                    tracing::debug!(?sent);
                }
            }
            .instrument(tracing::info_span!("hellohello_service")),
        );
        self
    }
}

#[tracing::instrument(level = tracing::Level::INFO, skip(chan))]
async fn hello_worker(worker: usize, chan: BiDi<HelloWorldRequest, HelloWorldResponse>) {
    tracing::debug!("hello world worker {worker} running...");
    while let Some(req) = chan.rx().recv().await {
        tracing::info!(?req);
        assert_eq!(req.hello, "hello");
        chan.tx()
            .send(HelloWorldResponse {
                world: "world".into(),
            })
            .await
            .unwrap();
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
    type RecvFrame = Vec<u8>;
    type Error = &'static str;

    async fn recv(&mut self) -> Result<Self::RecvFrame, &'static str> {
        let frame = self.rx.recv().await;
        tracing::info!(frame = ?frame.as_ref().map(HexSlice::new), "RECV");
        frame.ok_or("the send end of this wire has been dropped")
    }

    async fn send(&mut self, msg: OutboundFrame<'_>) -> Result<(), &'static str> {
        tracing::info!(?msg, "sending message");
        let frame = msg.to_vec().expect("message should serialize");
        tracing::info!(frame = ?HexSlice::new(&frame), "SEND");
        self.tx
            .send(frame)
            .await
            .map_err(|_| "the recv end of this wire has been dropped")
    }
}

// === impl TestFrame ===

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
