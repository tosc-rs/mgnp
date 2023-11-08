#![feature(async_fn_in_trait)]
#![cfg_attr(not(test), no_std)]

#[cfg(any(feature = "alloc", test))]
extern crate alloc;
use core::fmt;

use conn_table::ConnTable;
pub use conn_table::{Id, LinkId};
use connector::OutboundConnect;
use futures::FutureExt;
use tricky_pipe::{mpsc, oneshot, serbox};

// pub mod channel;
mod conn_table;
pub mod connector;
pub mod message;
pub mod registry;
pub use message::Frame;
use message::{InboundFrame, Nak, OutboundFrame};
pub use registry::Registry;
pub use tricky_pipe;

// pub trait Frame {
//     fn as_bytes(&self) -> &[u8];

//     fn decode(&self) -> postcard::Result<InboundMessage<'_>> {
//         postcard::from_bytes(self.as_bytes())
//     }
// }

/// Represents a wire-level transport for MGNP [`Frame`]s.
pub trait Wire {
    type Error: fmt::Display;
    type Buffer: AsMut<[u8]>;
    async fn send(&mut self, f: OutboundFrame<'_>) -> Result<(), Self::Error>;
    async fn recv(&mut self) -> Result<Self::Buffer, Self::Error>;
}

/// A MGNP network interface state machine for a particular [`Wire`].
///
/// This type implements the connection-management state machine for connections
/// over the provided `Wi`-typed [`Wire`] implementation. Local services are discovered using
/// the `R`-typed [`Registry`] implementation.
pub struct Machine<Wi, R, const MAX_CONNS: usize = { DEFAULT_MAX_CONNS }>
where
    // Remote wire type
    Wi: Wire,
{
    wire: Wi,
    conn_table: ConnTable<MAX_CONNS>,
    registry: R,
    conns_rx: mpsc::Receiver<OutboundConnect>,
}

#[derive(Clone)]
pub struct Interface(mpsc::Sender<OutboundConnect>);

/// Errors returned by [`Interface::run`].
#[derive(Debug)]
pub struct InterfaceError<E> {
    kind: InterfaceErrorKind<E>,
    ctx: Option<&'static str>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum InterfaceErrorKind<E> {
    /// A wire error occurred while sending a message.
    Send(E),

    /// A wire error occurred while receiving a message.
    Recv(E),

    /// A protocol error occurred.
    Proto(&'static str),
}

pub const DEFAULT_MAX_CONNS: usize = 512;

impl Interface {
    #[must_use]
    pub fn new<Wi, R, const MAX_CONNS: usize>(
        wire: Wi,
        registry: R,
        conns: mpsc::TrickyPipe<OutboundConnect>,
    ) -> (Self, Machine<Wi, R, MAX_CONNS>)
    where
        Wi: Wire,
        R: Registry,
    {
        let iface = Self(conns.sender());
        let worker = Machine {
            wire,
            conn_table: ConnTable::new(),
            registry,
            conns_rx: conns.receiver().unwrap(),
        };
        (iface, worker)
    }

    /// Returns a `Connector` that initiates connections for a particular
    /// service type.
    pub fn connector<S: registry::Service>(
        &self,
        hello_sharer: serbox::Sharer<S::Hello>,
        rsp: oneshot::Receiver<Result<(), Nak>>,
    ) -> connector::Connector<S> {
        connector::Connector {
            hello_sharer,
            rsp,
            tx: self.0.clone(),
        }
    }
}

impl<Wi, R, const MAX_CONNS: usize> Machine<Wi, R, MAX_CONNS>
where
    Wi: Wire,
    R: Registry,
{
    pub async fn run(&mut self) -> Result<(), InterfaceError<Wi::Error>> {
        use futures::future;

        let mut conns_remaining = true;
        let Self {
            conn_table,
            registry,
            wire,
            conns_rx,
        } = self;
        loop {
            let mut in_buf = None;
            let mut out_conn = None;
            // if the outbound connection stream has terminated, don't let it
            // wake us again.
            let next_conn = if conns_remaining {
                future::Either::Left(conns_rx.recv())
            } else {
                future::Either::Right(future::pending())
            };

            futures::select_biased! {
                // inbound frame from the wire.
                buf = wire.recv().fuse() => { in_buf = Some(buf.map_err(InterfaceError::recv)?); },

                // either a connection needs to send data, or a connection has
                // closed locally.
                frame = conn_table.next_outbound().fuse() => {
                    wire.send(frame).await.map_err(InterfaceError::send)?;
                    continue;
                },

                // locally-initiated connect request
                conn = next_conn.fuse() => {
                    if let Some(conn) = conn {
                        out_conn = Some(conn);
                    } else {
                        tracing::info!("connection stream has terminated");
                        conns_remaining = false;
                    };

                }
            };

            if let Some(mut buf) = in_buf {
                let frame = match InboundFrame::take_from_bytes(buf.as_mut()) {
                    Ok((f, _)) => f,
                    Err(e) => {
                        tracing::warn!(?e, "failed to decode inbound frame");
                        continue;
                    }
                };
                let id = frame.header.link_id();
                if id == LinkId::INTERFACE {
                    todo!("eliza: handle interface frame");
                    // return Ok(());
                }

                // process the inbound message, possibly generating a new outbound frame
                // to send.
                if let Some(rsp) = conn_table.process_inbound(&*registry, frame).await {
                    wire.send(rsp).await.map_err(|e| {
                        InterfaceError::send(e)
                            .with_context("while sending inbound frame response (ACK/NAK)")
                    })?;
                }
            } else if let Some(conn) = out_conn {
                tracing::debug!(identity = ?conn.identity, "initiating local connection...");
                match conn_table.start_connecting(conn) {
                    Some(frame) => {
                        wire.send(frame).await.map_err(|e| {
                            InterfaceError::send(e)
                                .with_context("while sending local-initiated CONNECT")
                        })?;
                        tracing::debug!("local connect sent!")
                    }
                    None => {
                        tracing::info!("refusing connection; no space in conn table!")
                        // TODO(eliza): tell the client that they can't have
                        // what hey want...
                    }
                }
            }
        }
    }
}

// === impl OutboundConnect ===

// === impl InterfaceError ===

impl<E> InterfaceError<E> {
    fn send(error: E) -> Self {
        Self {
            kind: InterfaceErrorKind::Send(error),
            ctx: None,
        }
    }

    fn recv(error: E) -> Self {
        Self {
            kind: InterfaceErrorKind::Recv(error),
            ctx: None,
        }
    }

    fn with_context(self, ctx: &'static str) -> Self {
        Self {
            ctx: Some(ctx),
            ..self
        }
    }

    #[must_use]
    pub fn kind(&self) -> &InterfaceErrorKind<E> {
        &self.kind
    }
}

impl<E: fmt::Display> fmt::Display for InterfaceError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { kind, ctx } = self;
        fmt::Display::fmt(kind, f)?;

        if let Some(ctx) = ctx {
            write!(f, " ({ctx})")?;
        }

        Ok(())
    }
}

// === impl InterfaceErrorKind ===

impl<E: fmt::Display> fmt::Display for InterfaceErrorKind<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Send(e) => write!(f, "wire send error: {e}"),
            Self::Recv(e) => write!(f, "wire receive error: {e}"),
            Self::Proto(e) => write!(f, "protocol error: {e}"),
        }
    }
}
