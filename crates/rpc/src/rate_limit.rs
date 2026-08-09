//! Per-client-IP rate limiting for the public JSON-RPC surface.
//!
//! `POST /rpc` and `GET /ws` are the only routes a caller can hit without
//! any cost to them but real cost to this node — every other route this
//! crate serves is either operator-facing (`/health`, `/metrics`) or
//! already content-addressed/self-bounding (the merged snapshot and
//! gateway routes). A client that can open unlimited connections or fire
//! unlimited JSON-RPC calls can single-handedly saturate the actor's
//! single dispatch queue for every other caller, which is the gap this
//! closes.
//!
//! # Mechanism
//!
//! Built on [`tower_governor`], which implements GCRA (a precise,
//! low-memory token-bucket variant: an initial burst of
//! [`RPC_RATE_BURST`] requests is allowed immediately, then one more is
//! admitted every `1000 / RPC_RATE_REFILL_PER_SEC` milliseconds) rather
//! than a hand-rolled bucket, keyed on
//! [`PeerIpKeyExtractor`] — the real TCP peer address from
//! [`axum::extract::ConnectInfo`], not an `X-Forwarded-For`-style header a
//! client can set to whatever it likes. Over budget gets a `429 Too Many
//! Requests` (`tower_governor`'s default `GovernorError` response), not a
//! silently dropped or delayed request.
//!
//! Only the `axum` feature of `tower_governor` is enabled
//! (`default-features = false` in `Cargo.toml`) — the crate's default
//! features additionally pull in `tonic`/gRPC support this server has no
//! use for.
//!
//! `ConnectInfo` only exists in a request's extensions if the server was
//! started with `.into_make_service_with_connect_info::<SocketAddr>()`
//! (wired in `openfiat-cli`'s `main.rs`) or, in tests, inserted by hand.
//! Without it, [`PeerIpKeyExtractor`] fails extraction and every request
//! gets a `500` — a loud failure, not a silent bypass of the limit.
//!
//! # Behind a reverse proxy
//!
//! Every request from behind a reverse proxy shares the proxy's socket
//! peer address, so v1 rate-limits the proxy as a whole rather than its
//! individual clients. Trusting `X-Forwarded-For` instead is a real
//! option (`tower_governor::key_extractor::SmartIpKeyExtractor`) but only
//! a safe one when the deployment can guarantee the header comes from a
//! trusted proxy and not directly from an attacker — that guarantee is a
//! deployment-topology fact this crate doesn't have, so it's left as a
//! follow-up rather than guessed at here.
//!
//! # Bounding the limiter's own memory
//!
//! The keyed rate limiter holds one bucket per IP ever seen, forever, if
//! nothing prunes it — which would make the limiter itself the memory-DoS
//! this task exists to close. [`wrap`] spawns a lazy background sweep
//! that periodically drops buckets that have been idle long enough to
//! have fully refilled (`RateLimiter::retain_recent`) and gives the
//! backing map a chance to release that freed capacity
//! (`RateLimiter::shrink_to_fit`).

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::PeerIpKeyExtractor;

/// Requests a single IP may send back-to-back before it starts getting
/// `429`s — the token bucket's capacity.
pub const RPC_RATE_BURST: u32 = 120;

/// Requests per second, per IP, refilled into that bucket once it's been
/// spent below capacity.
pub const RPC_RATE_REFILL_PER_SEC: u64 = 2;

/// How often the idle-entry sweep runs against the limiter's per-IP map.
/// Not itself a correctness knob (a longer interval only delays when
/// memory for a departed client's bucket is reclaimed), so it isn't one
/// of the two tuning constants above.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Wraps `router` — expected to be just the `/rpc` and `/ws` routes — in
/// the per-client-IP limiter described in the module doc.
///
/// Deliberately generic over the router's state type: this is called
/// before `with_state` resolves it (see `server::router`'s doc comment
/// on why), so the layer scopes to exactly the routes registered on
/// `router` and nothing merged in afterward.
pub(crate) fn wrap<S>(router: Router<S>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(PeerIpKeyExtractor)
            .per_millisecond(1000 / RPC_RATE_REFILL_PER_SEC)
            .burst_size(RPC_RATE_BURST)
            .finish()
            .expect(
                "RPC_RATE_BURST and the derived refill period are both non-zero by construction",
            ),
    );

    // `governor_conf.limiter()` is the shared `Arc<RateLimiter<..>>` the
    // layer below checks on every request; cloning it here (not the
    // config) is what lets the sweep run independently of any particular
    // request.
    let limiter = governor_conf.limiter().clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SWEEP_INTERVAL).await;
            limiter.retain_recent();
            limiter.shrink_to_fit();
        }
    });

    router.layer(GovernorLayer::new(governor_conf))
}
