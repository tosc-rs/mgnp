use crate::{registry::Identity, Id, LinkId};
use tricky_pipe::{mpsc::SerRecvRef, serbox};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum InboundMessage<'data> {
    Control(ControlMessage<&'data [u8]>),
    Data {
        local_id: Id,
        remote_id: Id,
        data: &'data [u8],
    },
}

#[derive(Debug)]
pub enum OutboundMessage<'data> {
    Control(ControlMessage<Option<serbox::Consumer>>),
    Data {
        local_id: Id,
        remote_id: Id,
        data: SerRecvRef<'data>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ControlMessage<T> {
    Ack {
        local_id: Id,
        remote_id: Id,
    },
    Nak {
        remote_id: Id,
        reason: Nak,
    },
    Connect {
        local_id: Id,
        identity: Identity,
        hello: T,
    },
    Reset {
        remote_id: Id,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Nak {
    ConnTableFull(usize),
    NotFound,
    Rejected(
        // TODO(eliza): can we cram a serialized message into this...?
    ),
}

impl InboundMessage<'_> {
    pub(crate) fn link_id(&self) -> LinkId {
        match self {
            Self::Control(msg) => msg.link_id(),
            Self::Data {
                local_id,
                remote_id,
                ..
            } => LinkId {
                local: Some(*local_id),
                remote: Some(*remote_id),
            },
        }
    }
}

impl OutboundMessage<'_> {
    pub(crate) fn reset(remote_id: Id) -> Self {
        Self::Control(ControlMessage::Reset { remote_id })
    }
}

impl<T> ControlMessage<T> {
    pub(crate) fn link_id(&self) -> LinkId {
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
        }
    }
}
