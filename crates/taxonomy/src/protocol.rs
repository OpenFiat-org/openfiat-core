//! Wire-level constants.
//!
//! A payment method carries OFS-2100's spec number, because a payment
//! method is part of the advertisement surface — it exists so an
//! advertisement can name what a buyer pays with, and nothing else reads
//! it. There is no OFS number of its own for the same reason
//! `openfiat-reviews` declined to invent one: a new spec range has to be
//! allocated in OFS-0000 before it can be claimed, and a number chosen
//! locally is how two crates end up gossiping under the same one.
//!
//! The event type is new within that spec, so it cannot collide with the
//! four `openfiat_advertisements::protocol` already defines.

pub const OFS_SPEC: u16 = 2100;

pub const EVENT_DEFINED: &str = "PaymentMethodDefined";
