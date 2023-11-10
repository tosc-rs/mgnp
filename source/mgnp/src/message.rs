use crate::{registry::Identity, Id, LinkId};
use serde::{Deserialize, Serialize};
use tricky_pipe::{mpsc::SerRecvRef, serbox};

/// A MGNP frame consists of a [message header](Header), followed by a message
/// body of zero or more bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame<T> {
    pub(crate) header: Header,
    pub(crate) body: T,
}

/// A `Header` describes the type of message contained by a [`Frame`], as well
/// as the link ID(s) of the connection that frame is associated with.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Header {
    /// Sent to acknowledge that a remote-initiated connection has been
    /// accepted by the server.
    ///
    /// An `ACK` frame is sent in response to a received
    /// [`CONNECT`](Self::Connect) frame once the service has accepted the new
    /// connection. Once the peer that initiated the connection has received the
    /// `ACK`, it may begin to send [`DATA`](Self::Data) frames on that connection.
    ///
    /// An `ACK` frame does not contain a body.
    Ack {
        /// The connection ID assigned to the new connection by the server.
        local_id: Id,
        /// The connection ID sent by the remote peer in its
        /// [`CONNECT`](Self::Connect) frame.
        remote_id: Id,
    },

    /// Sent to indicate that a remote-initiated connection could *not* be
    /// accepted by the server.
    ///
    /// An `NAK` frame is sent in response to a received
    /// [`CONNECT`](Self::Connect) frame, and indicates that the server could
    /// not accept the connection request. A future connection to the same
    /// service identity may be accepted by the server.
    ///
    /// If a `NAK` is received in response to a [`CONNECT`](Self::Connect)
    /// frame, a connection was not established, and the initiating peer should
    /// NOT attempt to send [`DATA``](Self::DATA) frames on that connection. The
    /// server MAY ignore any [`DATA`] frames with the `NAK`ed `remote_id`, or
    /// it MAY respond with [`RESET`](Self::Reset) frames.
    ///
    /// The initiating peer MAY reuse the ID of a `NAK`ed connection in a
    /// subsequent [`CONNECT`](Self::Connect) request to establish a new
    /// connection.
    ///
    /// The `NAK` frame contains a [`Nak`] value which indicates why the
    /// connection could not be successfully established. If the [`Nak`] value
    /// is [`Nak::Rejected`], the frame MAY contain a body containing a
    /// service-specific error value indicating why the connection was rejected
    /// by the service.
    Nak {
        /// The connection ID sent by the remote peer in its
        /// [`CONNECT`](Self::Connect) frame.
        remote_id: Id,
        /// Describes why the connection was not accepted.
        reason: Nak,
    },
    /// Sent to initiate a connection to a remote service.
    ///
    /// If the connection request
    Connect {
        local_id: Id,
        identity: Identity,
    },
    Reset {
        remote_id: Id,
    },
    Data {
        local_id: Id,
        remote_id: Id,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Nak {
    /// The connection was not accepted because the server cannot support any
    /// additional connections.
    ///
    /// If the initiating peer closes existing connections that it has initiated
    /// previously, the server MAY accept a new [`CONNECT`](Header::Connect)
    /// request from that peer. The connection limit is global to the server
    /// peer, and is not specific to the requested service [`Identity`].
    ConnTableFull(usize),
    /// No service matching the [`Identity`] provided in the
    /// [`CONNECT`](Header::Connect) frame exists on the server.
    ///
    /// This may indicate that the [`Service`](crate::registry::Service) UUID
    /// does not match any service running on the server, or that no instance of
    /// that service with the provided identity does not exist.
    ///
    /// This does not indicate that no service with the provided identity will
    /// NEVER exist on the server. A subsequent [`CONNECT`](Header::Connect)
    /// with the same [`Identity`] may succeed, if a service with the requested
    /// identity is later started.
    NotFound,
    /// The connection was rejected by the [`Service`](crate::registry::Service).
    ///
    /// The body of this [`NAK`](Header::Nak) frame may contain additional bytes
    /// which can be interpreted as a [service-specific `ConnectError`
    /// value](crate::registry::Service::ConnectError).]
    Rejected,
}

pub type InboundFrame<'data> = Frame<&'data [u8]>;
pub type OutboundFrame<'data> = Frame<OutboundData<'data>>;

#[derive(Debug)]
pub enum OutboundData<'recv> {
    Empty,
    Data(SerRecvRef<'recv>),
    Rejected(serbox::Consumer),
    Hello(serbox::Consumer),
}

impl Header {
    pub fn link_id(&self) -> LinkId {
        match *self {
            Self::Ack {
                local_id,
                remote_id,
            } => LinkId {
                local: Some(local_id),
                remote: Some(remote_id),
            },
            Self::Nak { remote_id, .. } => LinkId {
                local: None,
                remote: Some(remote_id),
            },
            Self::Connect { local_id, .. } => LinkId {
                local: Some(local_id),
                remote: None,
            },
            Self::Reset { remote_id } => LinkId {
                remote: Some(remote_id),
                local: None,
            },
            Self::Data {
                local_id,
                remote_id,
            } => LinkId {
                local: Some(local_id),
                remote: Some(remote_id),
            },
        }
    }

    /// Returns `true` if this `Header` describes a request with a body.
    fn has_body(&self) -> bool {
        matches!(
            self,
            Self::Nak {
                reason: Nak::Rejected,
                ..
            } | Self::Connect { .. }
                | Self::Data { .. }
        )
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum DecodeError {
    Header(postcard::Error),
    BodyFrame,
    Body(postcard::Error),
    UnexpectedTrailingData,
}

pub type DecodeResult<T> = Result<T, DecodeError>;

impl<'data> Frame<&'data [u8]> {
    pub fn from_bytes(frame: &'data [u8]) -> DecodeResult<Self> {
        let (hdr, rem) = postcard::take_from_bytes::<Header>(frame).map_err(DecodeError::Header)?;

        // if the header indicates that this message doesn't have a body, return
        // it now.
        if !hdr.has_body() {
            // if there's no body, there should be no remaining trailing data in
            // the frame.
            if !rem.is_empty() {
                return Err(DecodeError::UnexpectedTrailingData);
            }

            return Ok(Frame {
                header: hdr,
                body: &[],
            });
        }

        // otherwise, any trailing data is the body.
        let body = {
            let off = frame.len() - rem.len();
            &frame[off..]
        };

        Ok(Frame { header: hdr, body })
    }
}

impl<'bytes, T: Deserialize<'bytes>> Frame<T> {
    pub fn deserialize_from_bytes(buf: &'bytes mut [u8]) -> DecodeResult<Self> {
        let Frame { header, body } = Frame::<&[u8]>::from_bytes(buf)?;
        let body = postcard::from_bytes(body).map_err(DecodeError::Body)?;
        Ok(Frame { header, body })
    }
}

impl<'data> Frame<OutboundData<'data>> {
    pub fn data(remote_id: Id, local_id: Id, data: SerRecvRef<'data>) -> Self {
        Self {
            header: Header::Data {
                local_id,
                remote_id,
            },
            body: OutboundData::Data(data),
        }
    }

    pub fn ack(local_id: Id, remote_id: Id) -> Self {
        Self {
            header: Header::Ack {
                local_id,
                remote_id,
            },
            body: OutboundData::Empty,
        }
    }

    pub fn nak(remote_id: Id, reason: Nak) -> Self {
        Self {
            header: Header::Nak { remote_id, reason },
            body: OutboundData::Empty, // todo
        }
    }

    pub fn reset(remote_id: Id) -> Self {
        Self {
            header: Header::Reset { remote_id },
            body: OutboundData::Empty,
        }
    }

    pub fn connect(local_id: Id, identity: Identity, hello: serbox::Consumer) -> Self {
        Self {
            header: Header::Connect { local_id, identity },
            body: OutboundData::Hello(hello),
        }
    }

    pub fn to_slice<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        let Frame { header, body } = self;
        let mut used = postcard::to_slice(header, buf)?.len();

        let buf = &mut buf[used..];
        used += body.to_slice(buf)?.len();

        Ok(&mut buf[..used])
    }

    #[cfg(any(test, feature = "alloc"))]
    pub fn to_vec(&self) -> postcard::Result<alloc::vec::Vec<u8>> {
        let Frame { header, body } = self;
        let mut buf = postcard::to_allocvec(header)?;
        buf.append(&mut body.to_vec()?);
        Ok(buf)
    }
}

impl OutboundData<'_> {
    fn to_slice<'buf>(&self, buf: &'buf mut [u8]) -> postcard::Result<&'buf mut [u8]> {
        match self {
            Self::Empty => Ok(&mut []),
            Self::Data(data) => data.to_slice(buf),
            Self::Rejected(consumer) => consumer.to_slice(buf),
            Self::Hello(consumer) => consumer.to_slice(buf),
        }
    }

    #[cfg(any(test, feature = "alloc"))]
    fn to_vec(&self) -> postcard::Result<alloc::vec::Vec<u8>> {
        match self {
            Self::Empty => Ok(alloc::vec::Vec::new()),
            Self::Data(data) => data.to_vec(),
            Self::Rejected(consumer) => consumer.to_vec(),
            Self::Hello(consumer) => consumer.to_vec(),
        }
    }
}

// impl ControlMessage<serbox::Consumer> {
//     fn unpack(self) -> (ControlMessage<()>, Option<serbox::Consumer>) {
//         match self {
//             Self::Ack {
//                 local_id,
//                 remote_id,
//             } => (
//                 ControlMessage::Ack {
//                     local_id,
//                     remote_id,
//                 },
//                 None,
//             ),
//             Self::Nak { remote_id, reason } => (ControlMessage::Nak { remote_id, reason }, None),
//             Self::Connect {
//                 local_id,
//                 identity,
//                 hello,
//             } => (
//                 ControlMessage::Connect {
//                     local_id,
//                     identity,
//                     hello: (),
//                 },
//                 Some(hello),
//             ),
//             Self::Reset { remote_id } => (ControlMessage::Reset { remote_id }, None),
//         }
//     }

//     pub fn to_bytes(self, buf: &mut [u8]) -> postcard::Result<&mut [u8]> {
//         let (hdr, hello) = self.unpack();
//         let mut used = postcard::to_slice(&hdr, buf)?.len();

//         if let Some(hello) = hello {
//             let buf = &mut buf[used..];
//             used += hello.to_slice(buf)?.len();
//         }

//         Ok(&mut buf[..used])
//     }

//     #[cfg(any(test, feature = "alloc"))]
//     pub fn to_vec(self) -> postcard::Result<alloc::vec::Vec<u8>> {
//         let (hdr, hello) = self.unpack();
//         let mut buf = postcard::to_allocvec(&hdr)?;
//         if let Some(hello) = hello {
//             // XXX(eliza): this suuuuucks
//             buf.append(&mut hello.to_vec()?);
//         }

//         Ok(buf)
//     }
// }
