use crate::{registry::Identity, Id, LinkId};
use core::fmt;
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
    /// Sent to initiate a connection to a remote service.
    ///
    /// If the connection request
    Connect {
        local_id: Id,
        identity: Identity,
    },

    /// A data frame on an established connection.
    Data {
        local_id: Id,
        remote_id: Id,
    },

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
    /// A `REJECT` frame is sent in response to a received  [`CONNECT`] frame,
    /// and indicates that the server could not accept the connection request.
    /// A future connection to the same service identity may be accepted by the
    /// server.
    ///
    /// If a `REJECT` is received in response to a [`CONNECT`] frame, a
    /// connection was not established, and the initiating peer should
    /// NOT attempt to send [`DATA`] frames on that connection. The server MAY
    /// ignore any [`DATA`] frames with the `REJECT`ed `remote_id`, or it MAY
    /// respond with [`RESET`] frames.
    ///
    /// The initiating peer MAY reuse the ID of a `REJECT`ed connection in a
    /// subsequent [`CONNECT`]request to establish a new connection.
    ///
    /// The `REJECT` frame contains a [`Rejection`] value which indicates why the
    /// connection could not be successfully established. If the [`Rejection`] value
    /// is [`Rejection::ServiceRejected`], the frame MAY contain a body
    /// containing a [service-specific error value] indicating why the
    /// connection was rejected by the service.
    ///
    /// [`CONNECT`]: Self::Connect
    /// [`DATA`]: Self::Data
    /// [`RESET`]: Self::Reset
    /// [service-specific error value]: crate::Service::ConnectError
    Reject {
        /// The connection ID sent by the remote peer in its
        /// [`CONNECT`](Self::Connect) frame.
        remote_id: Id,
        /// Describes why the connection was not accepted.
        reason: Rejection,
    },

    Reset {
        remote_id: Id,
        reason: Reset,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Rejection {
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
    ServiceRejected,
    /// The connection was rejected because data could not be decoded.
    DecodeError(DecodeError),
}

/// Describes why a connection was reset.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Reset {
    /// The peer "just wanted to" reset the connection.
    ///
    /// This may because of an application-layer protocol error, or because the
    /// connection simply was no longer needed.
    BecauseISaidSo,
    /// The connection was reset because this peer could not decode a [`DATA`]
    /// frame received from the remote peer.
    ///
    /// Cyber police have been alerted.
    ///
    /// [`DATA`]: Header::Data
    YouDoneGoofed(DecodeError),
    /// A [`DATA`] frame or [`ACK`] was recieved on a connection that did not
    /// exist.
    ///
    /// [`DATA`]: Header::Data
    /// [`ACK`]: Header::Ack
    NoSuchConn,
    /// A [`CONNECT`] or [`ACK`] frame was recieved on a connection that was already
    /// established.
    ///
    /// [`CONNECT`]: Header::Connect
    /// [`ACK`]: Header::Ack
    YesSuchConn,
    /// The connection was reset because the peer is shutting down its MGNP
    /// interface on this wire.
    ///
    /// No further connections will be accepted, and you should go away forever.
    GoAway,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Nak {
    /// A frame was rejected because it could not be decoded successfully.
    DecodeError(DecodeError),
    /// The local ID sent by the remote does not exist.
    UnknownLocalId(Id),
    /// The remote ID sent by the remote does not correspond to an existing stream.
    UnknownRemoteId(Id),
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

#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DecodeError {
    Header(DecodeErrorKind),
    Body(DecodeErrorKind),
    UnexpectedTrailingData,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum DecodeErrorKind {
    /// Hit the end of buffer, expected more data
    UnexpectedEnd,
    /// Found a varint that didn't terminate. Is the usize too big for this platform?
    BadVarint,
    /// Found a bool that wasn't 0 or 1
    BadBool,
    /// Found an invalid unicode char
    BadChar,
    /// Tried to parse invalid utf-8
    BadUtf8,
    /// Found an Option discriminant that wasn't 0 or 1
    BadOption,
    /// Found an enum discriminant that was > u32::max_value()
    BadEnum,
    /// The original data was not well encoded
    BadEncoding,
    /// Bad CRC while deserializing
    BadCrc,
    /// Serde Deserialization Error
    SerdeDeCustom,
    /// Unknown errors.
    Unknown,
}

pub type DecodeResult<T> = Result<T, DecodeError>;

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

            Self::Reject { remote_id, .. } => LinkId {
                local: None,
                remote: Some(remote_id),
            },
            Self::Connect { local_id, .. } => LinkId {
                local: Some(local_id),
                remote: None,
            },
            Self::Reset { remote_id, .. } => LinkId {
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
            Self::Reject {
                reason: Rejection::ServiceRejected,
                ..
            } | Self::Connect { .. }
                | Self::Data { .. }
        )
    }
}

impl<'data> Frame<&'data [u8]> {
    pub fn from_bytes(frame: &'data [u8]) -> DecodeResult<Self> {
        let (hdr, rem) = postcard::take_from_bytes::<Header>(frame).map_err(DecodeError::header)?;

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
        let body = postcard::from_bytes(body).map_err(DecodeError::body)?;
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

    pub fn nak(remote_id: Id, reason: Rejection) -> Self {
        Self {
            header: Header::Reject { remote_id, reason },
            body: OutboundData::Empty, // todo
        }
    }

    pub fn reset(remote_id: Id, reason: Reset) -> Self {
        Self {
            header: Header::Reset { remote_id, reason },
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

// === impl Reset ===

impl Reset {
    pub(crate) fn bad_frame(error: postcard::Error) -> Self {
        Self::YouDoneGoofed(DecodeError::body(error))
    }
}

impl fmt::Display for Reset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::YouDoneGoofed(err) => write!(f, "you done goofed! received a bad frame: {err}"),
            Self::NoSuchConn => f.write_str("no such connection exists"),
            Self::YesSuchConn => f.write_str("connection already exists"),
            Self::GoAway => f.write_str("the peer is shutting down this interface, go away!"),
            Self::BecauseISaidSo => f.write_str("because i said so"),
        }
    }
}

// === impl DecodeError ===

impl DecodeError {
    fn body(error: postcard::Error) -> Self {
        Self::Body(DecodeErrorKind::from_postcard(error))
    }

    fn header(error: postcard::Error) -> Self {
        Self::Header(DecodeErrorKind::from_postcard(error))
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Body(err) => write!(f, "error decoding frame body: {err}"),
            Self::Header(err) => write!(f, "error decoding frame header: {err}"),
            Self::UnexpectedTrailingData => {
                f.write_str("unexpected trailing body data in a frame type without a body")
            }
        }
    }
}

// === impl DecodeErrorKind ===

impl DecodeErrorKind {
    fn from_postcard(error: postcard::Error) -> Self {
        match error {
            postcard::Error::DeserializeUnexpectedEnd => Self::UnexpectedEnd,
            postcard::Error::DeserializeBadVarint => Self::BadVarint,
            postcard::Error::DeserializeBadBool => Self::BadBool,
            postcard::Error::DeserializeBadChar => Self::BadChar,
            postcard::Error::DeserializeBadUtf8 => Self::BadUtf8,
            postcard::Error::DeserializeBadOption => Self::BadOption,
            postcard::Error::DeserializeBadEnum => Self::BadEnum,
            postcard::Error::DeserializeBadEncoding => Self::BadEncoding,
            postcard::Error::DeserializeBadCrc => Self::BadCrc,
            postcard::Error::SerdeDeCustom => Self::SerdeDeCustom,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for DecodeErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnexpectedEnd => "hit the end of the buffer, but expected more data",
            Self::BadVarint => {
                "found a varint that didn't terminate, is the usize too big for this platform?"
            }
            Self::BadBool => "found a bool that wasn't 0 or 1",
            Self::BadChar => "found an invalid unicode character",
            Self::BadUtf8 => "tried to parse invalid UTF-8",
            Self::BadOption => "found an Option discriminant that wasn't 0 or 1",
            Self::BadEnum => "found an enum discriminant that was > u32::max_value()",
            Self::BadEncoding => "the original data was not well encoded",
            Self::BadCrc => "bad CRC while deserializing",
            Self::SerdeDeCustom => "Serde deserializer error",
            Self::Unknown => "unknown error",
        })
    }
}
