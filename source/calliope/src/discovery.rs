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

pub struct EndpointListService;

impl Service for EndpointListService {
    type Hello = ListEndpoints;
    type ClientMsg = ();
    type ServerMsg = EndpointPage;
    type ConnectError = ();

    const UUID: Uuid = LIST_SERVICE_UUID;
}

pub const LIST_SERVICE_UUID: Uuid = uuid!("ec64bff3-7fc4-4ed5-a8f2-ed9e3b30f7be");

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ListEndpoints {
    /// List all endpoints available on the remote peer.
    All,
    /// List all endpoints that implement the provided service.
    Service(Uuid),
}

/// A single page of an endpoint list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EndpointPage {
    pub page_num: u32,
    pub pages: u32,
    pub bindings: heapless::Vec<Endpoint, { EndpointPage::PAGE_SIZE }>,
}

/// An endpoint binding, consisting of a [`service::Identity`] and metadata
/// describing the endpoint.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Endpoint {
    pub identity: service::Identity,
    pub binding: BindingKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum BindingKind {
    /// The endpoint is local to this peer.
    Local,
    // TODO(eliza): allow discovering remote endpoints
}

impl EndpointPage {
    pub const PAGE_SIZE: usize = 32;

    #[must_use]
    pub fn is_first(&self) -> bool {
        self.page_num == 0
    }

    #[must_use]
    pub fn is_last(&self) -> bool {
        self.page_num == self.pages - 1
    }
}
