//! Serving IPFS content from inside the node.
//!
//! # Why in-process rather than a daemon
//!
//! The first version of this pinned through Kubo over `--ipfs-api-url`.
//! That worked, and cost more than it looked: a second peer identity on
//! the network, a Go runtime and its resident memory alongside ours, and
//! an unauthenticated `/api/v0` control surface on port 5001 that lets
//! anyone who reaches it pin, unpin and read everything the daemon holds —
//! mitigated only by binding it to loopback. In-process there is no such
//! port, one identity, and one process to supervise.
//!
//! The deeper reason is the default. Running a daemon is work, so pinning
//! was opt-in, so almost nobody would have done it — and a durability
//! guarantee nobody opts into is not a guarantee. Serving from the node
//! itself is what lets it be on by default, which is what makes the
//! reward premium measure something real: with everyone serving, the
//! multiplier separates nodes that genuinely hold and answer for content
//! from those that are offline or have pruned, rather than separating
//! operators who bothered to install Go from those who did not.

pub mod message;
pub mod serve;
pub mod service;

pub use message::{Message, Presence, Want, WantType};
pub use serve::{BlockSource, PROTOCOL, respond};
pub use service::{spawn_inbound, spawn_send, wantlist};
