use anchor_lang::prelude::*;

use crate::state::BanReason;

/// OFS-7100 §12.2 requires listing *and* delisting to emit events: "An
/// exclusion nobody can audit or reverse is a worse failure than the
/// abuse it prevents." These two events are the audit half of that
/// sentence; `delist_wallet` is the reversible half.
#[event]
pub struct WalletListed {
    pub wallet: Pubkey,
    pub reason: BanReason,
    pub evidence_hash: [u8; 32],
    pub listed_by: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct WalletDelisted {
    pub wallet: Pubkey,
    pub delisted_by: Pubkey,
    /// When the wallet was originally listed, carried over from the
    /// record being closed. Without it the delisting event alone cannot
    /// tell an auditor how long access was withheld — and the record
    /// that knew is gone by the time this is read.
    pub listed_at: i64,
    pub timestamp: i64,
}
