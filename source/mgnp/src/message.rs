use crate::{registry::Identity, Id, LinkId};
use serde::{Deserialize, Serialize};
use tricky_pipe::{mpsc::SerRecvRef, serbox};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Header {
    Ack { local_id: Id, remote_id: Id },
    Nak { remote_id: Id, reason: Nak },
    Connect { local_id: Id, identity: Identity },
    Reset { remote_id: Id },
    Data { local_id: Id, remote_id: Id },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame<T> {
    pub(crate) header: Header,
    pub(crate) body: T,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Nak {
    ConnTableFull(usize),
    NotFound,
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

    fn has_body(&self) -> bool {
        match self {
            Self::Nak {
                reason: Nak::Rejected,
                ..
            } => true,
            Self::Connect { .. } => true,
            Self::Data { .. } => true,
            _ => false,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum DecodeError {
    Header(postcard::Error),
    BodyFrame,
    Body(postcard::Error),
}

pub type DecodeResult<T> = Result<T, DecodeError>;

impl<'data> Frame<&'data [u8]> {
    pub fn take_from_bytes(buf: &'data mut [u8]) -> DecodeResult<(Self, &'data mut [u8])> {
        let (hdr, rem) = postcard::take_from_bytes::<Header>(buf).map_err(DecodeError::Header)?;
        let buf = {
            let off = buf.len() - rem.len();
            &mut buf[off..]
        };

        // if the header indicates that this message doesn't have a body, return
        // it now.
        if !hdr.has_body() {
            return Ok((
                Frame {
                    header: hdr,
                    body: &[],
                },
                buf,
            ));
        }

        // otherwise, take the body...
        let (body, buf) = {
            let len = cobs::decode_in_place(buf).map_err(|_| DecodeError::BodyFrame)?;
            buf.split_at_mut(len)
        };
        Ok((Frame { header: hdr, body }, buf))
    }
}

impl<'bytes, T: Deserialize<'bytes>> Frame<T> {
    pub fn deserialize_from_bytes(buf: &'bytes mut [u8]) -> DecodeResult<(Self, &mut [u8])> {
        let (Frame { header, body }, buf) = Frame::<&[u8]>::take_from_bytes(buf)?;
        let body = postcard::from_bytes(body).map_err(DecodeError::Body)?;
        Ok((Frame { header, body }, buf))
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
            Self::Data(data) => data.to_slice_framed(buf),
            Self::Rejected(consumer) => consumer.to_slice_framed(buf),
            Self::Hello(consumer) => consumer.to_slice_framed(buf),
        }
    }

    #[cfg(any(test, feature = "alloc"))]
    fn to_vec(&self) -> postcard::Result<alloc::vec::Vec<u8>> {
        match self {
            Self::Empty => Ok(alloc::vec::Vec::new()),
            Self::Data(data) => data.to_vec_framed(),
            Self::Rejected(consumer) => consumer.to_vec_framed(),
            Self::Hello(consumer) => consumer.to_vec_framed(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;
    use std::dbg;
    use uuid::{uuid, Uuid};

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct SerStruct<'a> {
        a: usize,
        b: usize,
        s: &'a str,
    }

    const HELLO_WORLD_UUID: Uuid = uuid!("6c5361c3-cb70-4651-9c6e-8dd3e3625910");

    fn hello_world_id() -> registry::Identity {
        registry::Identity {
            id: HELLO_WORLD_UUID,
            kind: registry::IdentityKind::Name("helloworld".into()),
        }
    }

    #[test]
    fn hello_to_vec_works() {
        let hello = dbg!(SerStruct {
            a: 69,
            b: 420,
            s: "hello",
        });

        // let hello_bytes = dbg!(postcard::to_allocvec(&hello).unwrap());
        // let mut sharer = serbox::Sharer::new();
        // let consumer = sharer.try_share(hello.clone()).unwrap();

        // let connect = dbg!(ControlMessage::Connect {
        //     local_id: Id::new(1),
        //     identity: hello_world_id(),
        //     hello: consumer,
        // });
        // let connect_bytes = dbg!(connect.to_vec()).unwrap();
        // let deser_connect: ControlMessage<&[u8]> = dbg!(postcard::from_bytes(&connect_bytes[..]))
        //     .expect("serialized control message should deserialize");

        // match deser_connect {
        //     ControlMessage::Connect {
        //         local_id,
        //         identity,
        //         hello,
        //     } => {
        //         assert_eq!(local_id, Id::new(1));
        //         assert_eq!(identity, hello_world_id());
        //         assert_eq!(hello, hello_bytes.as_slice());
        //         assert_eq!(dbg!(postcard::from_bytes(&hello_bytes[..])), Ok(hello));
        //     }
        //     msg => panic!("deserialized control message should be a Connect; got {msg:#?}"),
        // }
    }
}
