//! Test fixtures shared across this crate's modules.
//!
//! These are not invented strings. [`PROBE_CID`] is what Filebase's IPFS
//! RPC returned for a file this project actually uploaded, and the same
//! identifier retrieves it from `ipfs.io` — an unrelated gateway, with no
//! credentials, which is the observation that made `record`'s "anyone can
//! read this" documentation a checked fact rather than an assumption.
//! `openfiat_crypto::cid` holds the bytes and checks the digest against
//! them; here only the identifiers are needed.

use crate::Cid;

pub const PROBE_CID: &str = "bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva";

/// A second, different CID, for tests that need two.
pub const OTHER_CID: &str = "bafkreibqyjcrlslvz3uen3qjl6gaqyxu2tryyvqlb555rluyyszpg5zbqu";

pub fn probe_cid() -> Cid {
    Cid::parse(PROBE_CID).expect("the probe CID is real")
}

pub fn other_cid() -> Cid {
    Cid::parse(OTHER_CID).expect("the second CID is real")
}
