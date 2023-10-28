#![feature(async_fn_in_trait)]
#![cfg_attr(not(test), no_std)]

#[cfg(any(feature = "alloc", test))]
extern crate alloc;

use conn_table::ConnTable;
pub use conn_table::{Id, LinkId};
use futures::{FutureExt, Stream, StreamExt};
use tricky_pipe::bidi::SerBiDi;

mod conn_table;
pub mod message;
pub mod registry;
pub use message::Message;
use message::{ControlMessage, Nak};
pub use registry::Registry;
pub use tricky_pipe;

pub trait Frame {
    fn as_bytes(&self) -> &[u8];

    fn decode(&self) -> postcard::Result<Message<'_>> {
        postcard::from_bytes(self.as_bytes())
    }
}

pub trait Wire {
    type Frame: Frame;
    async fn send(&mut self, f: Message<'_>) -> Result<(), ()>;
    async fn recv(&mut self) -> Result<Self::Frame, ()>;
}

/// An outbound connect request.
pub struct OutboundConnect<'data> {
    /// The identity of the remote service to connect to.
    identity: registry::Identity,
    /// The "hello" message to send to the remote service.
    hello: &'data [u8],
    /// The local bidirectional channel to bind to the remote service.
    channel: SerBiDi,
    // TODO(eliza): add a oneshot for "connect success/connect failed"...
}

pub struct Interface<Fr, Wi, R, const MAX_CONNS: usize = { DEFAULT_MAX_CONNS }>
where
    Fr: Frame,
    // Remote wire type
    Wi: Wire<Frame = Fr>,
{
    wire: Wi,
    conn_table: ConnTable<MAX_CONNS>,
    registry: R,
}

pub const DEFAULT_MAX_CONNS: usize = 512;

impl<Fr, Wi, R, const MAX_CONNS: usize> Interface<Fr, Wi, R, MAX_CONNS>
where
    Fr: Frame,
    Wi: Wire<Frame = Fr>,
    R: Registry,
{
    #[must_use]
    pub fn new(wire: Wi, registry: R) -> Self {
        Self {
            wire,
            conn_table: ConnTable::new(),
            registry,
        }
    }

    pub async fn run(&mut self, conns: impl Stream<Item = OutboundConnect<'_>>) -> Result<(), ()> {
        futures::pin_mut!(conns);
        loop {
            futures::select_biased! {
                // inbound frame from the wire.
                frame = self.wire.recv().fuse() => self.process_inbound(frame?).await?,
                // either a connection needs to send data, or a connection has
                // closed locally.
                frame = self.conn_table.next_outbound().fuse() => {
                    self.wire.send(frame).await?;
                },

                // locally-initiated connect request
                conn = conns.next().fuse() => {
                    let Some(OutboundConnect { identity, hello, channel, .. }) = conn else {
                        tracing::info!("connection stream has terminated");
                        // TODO(eliza): should the run loop die here instead?
                        continue;
                    };

                    tracing::debug!(?identity, "initiating local connection...");
                    match self.conn_table.start_connecting(identity, hello, channel) {
                        Some(frame) => {
                            self.wire.send(Message::Control(frame)).await?;
                            tracing::debug!("local connect sent!")
                        }
                        None => {
                            tracing::info!("refusing connection; no space in conn table!")
                            // TODO(eliza): tell the client that they can't have
                            // what hey want...
                        },
                    }

                }
            };
        }
    }

    async fn process_inbound(&mut self, frame: Fr) -> Result<(), ()> {
        let msg = match frame.decode() {
            Ok(msg) => msg,
            Err(error) => {
                tracing::warn!(%error, "failed to decode inbound frame");
                // return "Ok" here, even though there was an error, because we
                // want the interface loop to keep running, instead of dying.
                return Ok(());
            }
        };

        let id = msg.link_id();
        if id == LinkId::INTERFACE {
            todo!("eliza: handle interface frame");
            // return Ok(());
        }

        // process the inbound message, possibly generating a new outbound frame
        // to send.
        if let Some(rsp) = self.conn_table.process_inbound(&self.registry, msg).await {
            self.wire.send(rsp).await?;
        }

        Ok(())
    }
}

// === impl OutboundConnect ===

impl<'data> OutboundConnect<'data> {
    #[must_use]
    pub fn new(identity: registry::Identity, hello: &'data [u8], channel: SerBiDi) -> Self {
        Self {
            identity,
            hello,
            channel,
        }
    }
}
