//! Wire-level constants.
//!
//! A review carries the Reputation Engine's spec number, because that is
//! the surface it appears on. There is no OFS number of its own: a new
//! spec range would have to be allocated in OFS-0000 before one could be
//! claimed here, and inventing a number locally is how two crates end up
//! gossiping under the same one.
//!
//! OFS-3000 is unclaimed by any existing event type — `openfiat-reputation`
//! deliberately originates nothing at all (see that crate's `view` module)
//! — so nothing else can collide with what is defined here.

pub const OFS_SPEC: u16 = 3000;

pub const EVENT_PUBLISHED: &str = "ReviewPublished";
