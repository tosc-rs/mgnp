use crate::{
    message::{OutboundFrame, Rejection, Reset},
    registry::{self, Registry},
    tricky_pipe::{
        bidi::{BiDi, SerBiDi},
        mpsc::TrickyPipe,
    },
    Interface, Wire,
};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, RwLock},
};
use tokio::sync::{mpsc, oneshot, Notify};
pub use tracing::Instrument;

mod e2e;
mod integration;

pub(crate) mod svcs {
    use super::*;
    use crate::registry;
    use uuid::{uuid, Uuid};

    pub struct HelloWorld;

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

    impl registry::Service for HelloWorld {
        type ClientMsg = HelloWorldRequest;
        type ServerMsg = HelloWorldResponse;
        type ConnectError = ();
        type Hello = ();
        const UUID: Uuid = uuid!("6c5361c3-cb70-4651-9c6e-8dd3e3625910");
    }

    pub struct HelloWithHello;

    impl registry::Service for HelloWithHello {
        type ClientMsg = HelloWorldRequest;
        type ServerMsg = HelloWorldResponse;
        type ConnectError = ();
        type Hello = HelloHello;
        const UUID: Uuid = uuid!("9442b293-93d8-48b9-bbf7-52f636462bfe");
    }

    pub fn hello_with_hello_id() -> registry::Identity {
        registry::Identity::from_name::<HelloWithHello>("hello-hello")
    }

    pub fn hello_world_id() -> registry::Identity {
        registry::Identity::from_name::<HelloWorld>("hello-world")
    }

    #[tracing::instrument(level = tracing::Level::INFO, skip(conns))]
    pub async fn serve_hello(
        req_msg: &'static str,
        rsp_msg: &'static str,
        mut conns: mpsc::Receiver<InboundConnect>,
    ) {
        let mut worker = 1;
        while let Some(req) = conns.recv().await {
            let InboundConnect { hello, rsp } = req;
            tracing::info!(?hello, "hello world service received connection");
            let (their_chan, my_chan) =
                make_bidis::<svcs::HelloWorldRequest, svcs::HelloWorldResponse>(8);
            tokio::spawn(hello_worker(worker, req_msg, rsp_msg, my_chan));
            worker += 1;
            let sent = rsp.send(Ok(their_chan)).is_ok();
            tracing::debug!(?sent);
        }
    }

    #[tracing::instrument(level = tracing::Level::INFO, skip(chan))]
    pub(super) async fn hello_worker(
        worker: usize,
        req_msg: &'static str,
        rsp_msg: &'static str,
        chan: BiDi<svcs::HelloWorldRequest, svcs::HelloWorldResponse, Reset>,
    ) {
        tracing::debug!("hello world worker {worker} running...");
        while let Ok(req) = chan.rx().recv().await {
            tracing::info!(?req);
            assert_eq!(req.hello, req_msg);
            chan.tx()
                .send(svcs::HelloWorldResponse {
                    world: rsp_msg.into(),
                })
                .await
                .unwrap();
        }
    }
}

pub struct Fixture<L, R> {
    local: L,
    remote: R,
    test_done: Arc<Notify>,
}

impl Fixture<Option<TestWire>, Option<TestWire>> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for Fixture<Option<TestWire>, Option<TestWire>> {
    fn default() -> Self {
        trace_init();
        let (local, remote) = TestWire::new();
        Self {
            local: Some(local),
            remote: Some(remote),
            test_done: Arc::new(Notify::new()),
        }
    }
}

type Running = (Interface, tokio::task::JoinHandle<()>);

impl<T, E> Fixture<T, E> {
    fn spawn_peer(
        name: &'static str,
        wire: TestWire,
        registry: TestRegistry,
        test_done: &Arc<Notify>,
    ) -> Running {
        let (iface, machine) = Interface::new::<_, _, { crate::DEFAULT_MAX_CONNS }>(
            wire,
            registry,
            TrickyPipe::new(8),
        );
        let task = tokio::spawn(interface("name", machine, test_done.clone()));
        (iface, task)
    }
}

impl<R> Fixture<Option<TestWire>, R> {
    pub fn spawn_local(self, registry: TestRegistry) -> Fixture<Running, R> {
        let Fixture {
            mut local,
            remote,
            test_done,
        } = self;
        let wire = local
            .take()
            .expect("attempted to take the local end of the wire twice!");
        Fixture {
            local: Self::spawn_peer("local", wire, registry, &test_done),
            remote,
            test_done,
        }
    }

    pub fn take_local_wire(&mut self) -> TestWire {
        self.local
            .take()
            .expect("attempted to take the local end of the wire twice!")
    }
}

impl<L> Fixture<L, Option<TestWire>> {
    pub fn spawn_remote(self, registry: TestRegistry) -> Fixture<L, Running> {
        let Fixture {
            local,
            mut remote,
            test_done,
        } = self;

        let wire = remote
            .take()
            .expect("attempted to take the remote end of the wire twice!");
        Fixture {
            local,
            remote: Self::spawn_peer("local", wire, registry, &test_done),
            test_done,
        }
    }

    pub fn take_remote_wire(&mut self) -> TestWire {
        self.remote
            .take()
            .expect("attempted to take the remote end of the wire twice!")
    }
}

impl<L> Fixture<L, Running> {
    #[allow(dead_code)]
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
    mut machine: crate::Machine<TestWire, TestRegistry, { crate::DEFAULT_MAX_CONNS }>,
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

pub fn make_bidis<In, Out>(cap: u8) -> (SerBiDi<Reset>, BiDi<In, Out, Reset>)
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
    pub rsp: oneshot::Sender<Result<SerBiDi<Reset>, Rejection>>,
}

// === impl TestRegistry ===

impl Registry for TestRegistry {
    #[tracing::instrument(level = tracing::Level::INFO, name = "Registry::connect", skip(self, hello),)]
    async fn connect(
        &self,
        identity: registry::Identity,
        hello: &[u8],
    ) -> Result<SerBiDi<Reset>, Rejection> {
        let Some(svc) = self.svcs.read().unwrap().get(&identity).cloned() else {
            tracing::info!("REGISTRY: service not found!");
            return Err(Rejection::NotFound);
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
            return Err(Rejection::NotFound);
        }

        rx.await.map_err(|_| Rejection::NotFound)?
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
        let conns = self.add_service(svcs::hello_world_id());
        tokio::spawn(svcs::serve_hello("hello", "world", conns));
        self
    }

    pub fn spawn_hello_with_hello(&self) -> &Self {
        let mut chan = self.add_service(svcs::hello_with_hello_id());
        tokio::spawn(
            async move {
                let mut worker = 1;
                while let Some(req) = chan.recv().await {
                    let InboundConnect { hello, rsp } = req;
                    tracing::info!(?hello, "hellohello service received connection");
                    let hello = postcard::from_bytes::<svcs::HelloHello>(&hello)
                        .expect("hellohello message must deserialize!");
                    let res = if hello.hello == "hello" {
                        tracing::info!(?hello, "hellohello service received hello");
                        let (their_chan, my_chan) =
                            make_bidis::<svcs::HelloWorldRequest, svcs::HelloWorldResponse>(8);
                        tokio::spawn(svcs::hello_worker(worker, "hello", "world", my_chan));
                        worker += 1;
                        Ok(their_chan)
                    } else {
                        tracing::info!(
                            ?hello,
                            "hellohello service received valid non-matching hello!"
                        );
                        Err(Rejection::ServiceRejected)
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

// === impl TestWire ===

impl TestWire {
    pub fn new() -> (Self, Self) {
        let (tx1, rx1) = mpsc::channel(8);
        let (tx2, rx2) = mpsc::channel(8);
        (Self { tx: tx1, rx: rx2 }, Self { tx: tx2, rx: rx1 })
    }

    pub async fn send_bytes(&mut self, frame: impl Into<Vec<u8>>) -> Result<(), &'static str> {
        let frame = frame.into();
        tracing::info!(frame = ?HexSlice::new(&frame), "SEND");
        self.tx
            .send(frame)
            .await
            .map_err(|_| "the recv end of this wire has been dropped")
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
        self.send_bytes(frame).await
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

#[tracing::instrument(level = tracing::Level::INFO, skip(connector, hello))]
pub async fn connect_should_nak<S: registry::Service>(
    connector: &mut crate::Connector<S>,
    name: &'static str,
    hello: S::Hello,
    nak: crate::message::Rejection,
) {
    tracing::info!("connecting to {name} (should NAK)...");
    let res = connector
        .connect(name, hello, crate::client::Channels::new(8))
        .await;
    tracing::info!(?res, "connect result");
    match res {
        Err(crate::client::ConnectError::Nak(actual)) => assert_eq!(
            actual, nak,
            "expected connection to {name} to be NAK'd with {nak:?}, but it was NAK'd with {actual:?}!"
        ),
        Err(error) => panic!(
            "expected connection to {name} to be NAK'd with {nak:?}, but it failed with {error:?}!"
        ),
        Ok(_) => {
            panic!("expected connection to {name} to be NAK'd with {nak:?}, but it succeeded!")
        }
    }
}

#[tracing::instrument(level = tracing::Level::INFO, skip(connector, hello))]
pub async fn connect<S: registry::Service>(
    connector: &mut crate::Connector<S>,
    name: &'static str,
    hello: S::Hello,
) -> crate::client::Connection<S> {
    tracing::info!("connecting to {name} (should SUCCEED)...");
    let res = connector
        .connect(name, hello, crate::client::Channels::new(8))
        .await;
    tracing::info!(?res, "connect result");
    match res {
        Ok(ch) => ch,
        Err(error) => {
            panic!("expected connection to {name} to succeed, but it failed with {error:?}")
        }
    }
}
