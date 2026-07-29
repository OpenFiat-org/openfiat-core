use anchor_lang::prelude::*;

use crate::state::BanReason;

/// OFS-7100 §12.2 requires listing *and* delisting to emit events: "An
/// exclusion nobody can audit or reverse is a worse failure than the
/// abuse it prevents." These two events are the audit half of that
/// sentence; `delist_wallet` is the reversible half.
///
/// Both name the authorizing proposal rather than a signer. There is no
/// signer worth naming any more: the transaction may be submitted by
/// anyone, and the decision belonged to the vote.
#[event]
pub struct WalletListed {
    pub wallet: Pubkey,
    pub reason: BanReason,
    pub evidence_hash: [u8; 32],
    /// The `Proposal` whose accepted vote authorized this listing.
    pub authorizing_proposal: Pubkey,
    pub proposal_id: u64,
    pub timestamp: i64,
}

#[event]
pub struct WalletDelisted {
    pub wallet: Pubkey,
    /// The `Proposal` whose accepted vote authorized the readmission —
    /// necessarily a different proposal from the one that listed the
    /// wallet, reached through exactly the same machinery.
    pub authorizing_proposal: Pubkey,
    pub proposal_id: u64,
    /// When the wallet was originally listed, carried over from the
    /// record being closed. Without it the delisting event alone cannot
    /// tell an auditor how long access was withheld — and the record
    /// that knew is gone by the time this is read.
    pub listed_at: i64,
    /// The proposal that listed the wallet, likewise carried over before
    /// the record holding it is closed. The pair is what lets an auditor
    /// set the exclusion decision beside the readmission one.
    pub listed_by_proposal: Pubkey,
    pub timestamp: i64,
}
