//! Calliope service discovery protocol.
//!
//! This module contains a [`Service`] definition, [`EndpointListService`], and
//! messages used by that [`Service`]. [`EndpointListService`] provides a
//! mechanism for a client to discover the endpoints available on its remote
//! peer.
//!
//! In general, this service should be implemented by a peer using its
//! [`service::Registry`] type.
use crate::service::{self, Service};
use serde::{Deserialize, Serialize};
use uuid::{uuid, Uuid};

pub struct EndpointsService;

impl Service for EndpointsService {
    type Hello = WatchEndpoints;
    type ClientMsg = ();
    type ServerMsg = EndpointsPage;
    type ConnectError = ();

    const UUID: Uuid = LIST_SERVICE_UUID;
}

pub const LIST_SERVICE_UUID: Uuid = uuid!("ec64bff3-7fc4-4ed5-a8f2-ed9e3b30f7be");

/// Request type for [`EndpointListService`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum WatchEndpoints {
    /// Stream all endpoints available on the remote peer.
    All,
    /// Stream all endpoints that implement the provided service.
    Service(Uuid),
}

/// A set of endpoint updates.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EndpointsPage {
    pub endpoints: heapless::Vec<Update, { EndpointsPage::PAGE_SIZE }>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Update {
    /// A new endpoint should be added to the remote peer's endpoint list.
    Add(EndpointBinding),
    /// A [`service::Identity`] no longer exists on this peer.
    Remove(service::Identity),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EndpointBinding {
    /// The identity of the endpoint.
    pub identity: service::Identity,
    /// How this endpoint is routed to by this peer.
    pub kind: EndpointKind,
}

/// Describes how an endpoint is routed to by this peer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EndpointKind {
    /// The endpoint is local to this peer.
    Local,
    // TODO(eliza): allow discovering remote endpoints
}

impl EndpointsPage {
    pub const PAGE_SIZE: usize = 32;
}
