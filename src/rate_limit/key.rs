//! How a request is attributed to a caller for rate-limiting purposes.

use std::net::IpAddr;
use tower_governor::errors::GovernorError;
use tower_governor::key_extractor::{KeyExtractor, SmartIpKeyExtractor};

/// The bucket a request with no determinable client IP falls into.
///
/// Every such request shares one bucket, deliberately: an unattributable flood is exactly
/// the traffic we least want to let through, and sharing a bucket makes it self-limiting.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum ClientKey {
    Ip(IpAddr),
    Unattributed,
}

/// Per-client key for the auth limiters.
///
/// Wraps governor's [`SmartIpKeyExtractor`], which reads `X-Forwarded-For`, then `X-Real-IP`,
/// then `Forwarded`, before falling back to the peer address.
///
/// **This trusts the ingress.** Those headers are client-controllable on any request that
/// reaches the pod without passing through the load balancer, so an attacker with direct
/// access to the pod IP can rotate `X-Forwarded-For` and mint a fresh bucket per request.
/// The service is only ever reachable through the cluster ingress, which overwrites
/// `X-Forwarded-For` with the real peer, so that is a containment property of the network,
/// not of this code. If the service is ever exposed directly, this must become a
/// trusted-proxy-aware extractor instead.
///
/// The difference from using `SmartIpKeyExtractor` directly is the failure mode: it errors
/// when it can find no IP at all, and tower_governor turns that error into a `500`. Nothing
/// stamps `ConnectInfo` on requests here (the server is started with a plain
/// `axum::serve`), so a request arriving without any forwarding header would fail every
/// call. Answering `500` to traffic we merely could not attribute is worse than metering it,
/// so those requests land in [`ClientKey::Unattributed`] together.
#[derive(Clone, Copy, Debug)]
pub struct ClientIpKeyExtractor;

impl KeyExtractor for ClientIpKeyExtractor {
    type Key = ClientKey;

    fn name(&self) -> &'static str {
        "client IP"
    }

    fn extract<T>(&self, req: &axum::http::Request<T>) -> Result<Self::Key, GovernorError> {
        Ok(SmartIpKeyExtractor
            .extract(req)
            .map(ClientKey::Ip)
            .unwrap_or(ClientKey::Unattributed))
    }

    fn key_name(&self, key: &Self::Key) -> Option<String> {
        match key {
            ClientKey::Ip(ip) => Some(ip.to_string()),
            ClientKey::Unattributed => Some("unattributed".to_string()),
        }
    }
}
