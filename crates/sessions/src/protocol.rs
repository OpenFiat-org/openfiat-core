//! Wire-level constants. `SessionEstablished` is drawn directly from
//! OFS-8100 (OETR)'s Peer/Network Events; OFS-1400 never names its
//! renew/revoke/migrate events explicitly (§13/§16/§12 describe the
//! *behavior* only), and OETR doesn't cover them either, so those three
//! are minted in the same PascalCase convention (matching the precedent
//! set by `openfiat-oracles`'s `OracleRecordPublished`). `SessionExpired`
//! (also in OETR) isn't emitted as a signed event — like every other
//! expiry in this workspace, it's computed locally from `expires_at`,
//! not broadcast.

use std::time::Duration;

pub const OFS_SPEC: u16 = 1400;

pub const EVENT_ESTABLISHED: &str = "SessionEstablished";
pub const EVENT_RENEWED: &str = "SessionRenewed";
pub const EVENT_REVOKED: &str = "SessionRevoked";
pub const EVENT_MIGRATED: &str = "SessionMigrated";

/// `[PROPOSED — NEEDS SIGN-OFF]`: OFS-1400 leaves session lifetime to
/// implementations (§14 only lists expiration *criteria*, not a value).
pub const DEFAULT_SESSION_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
