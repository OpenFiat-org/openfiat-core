//! Wire-level constants.
//!
//! Attachments carry the Settlement & Liquidity spec number because that
//! is what they attach to. There is no OFS number of their own: a new
//! spec range would have to be allocated in OFS-0000 before one could be
//! claimed here, and inventing a number locally is how two crates end up
//! gossiping under the same one.

pub const OFS_SPEC: u16 = 5000;

pub const EVENT_PUBLISHED: &str = "AttachmentPublished";
