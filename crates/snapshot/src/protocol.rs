//! Wire-level constants. §12: "snapshot providers advertise newly
//! available snapshots through the Gossip Protocol. Only metadata is
//! gossiped" — `CheckpointCreated` (OFS-8100/OETR) is this crate's own
//! event, since a new snapshot's metadata is exactly a new checkpoint.
//! The request/receive/verify/reject download cycle (§13-17) doesn't
//! travel over gossip at all — it's a private conversation with whichever
//! provider a node chooses (§14's transport is out of this crate's
//! scope), so those OETR names aren't emitted as gossip events here.

pub const OFS_SPEC: u16 = 1300;

pub const EVENT_ANNOUNCED: &str = "CheckpointCreated";

/// `[PROPOSED — NEEDS SIGN-OFF]`: the protocol/schema version this
/// implementation's snapshots are produced under and can import.
///
/// Bumped 1 → 2 when `SnapshotMetadata::locations` entered the signed
/// announcement payload. Version 1 announcements can no longer verify
/// (see `crate::events`' module doc for that decision), and this bump is
/// what makes `import` reject them as `UnsupportedProtocolVersion` — a
/// mismatch an operator can act on — rather than as `InvalidSignature`,
/// which would send them looking for an attacker. A version 1 snapshot
/// carried no location and so was never fetchable in the first place;
/// nothing importable is lost.
pub const SUPPORTED_PROTOCOL_VERSION: u32 = 2;
