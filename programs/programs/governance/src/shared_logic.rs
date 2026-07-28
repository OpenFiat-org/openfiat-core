//! Category-to-threshold lookup — shared by `create_proposal` (which
//! snapshots these onto the `Proposal`) so the mapping lives in one
//! place.

use openfiat_programs_shared::ProposalCategory;

use crate::state::GovernanceConfig;

/// OFS-4100 §5: standard categories use `quorum_bps`; Protocol-Upgrade
/// and Constitutional use the higher `quorum_upgrade_bps`.
pub fn quorum_bps_for(config: &GovernanceConfig, category: ProposalCategory) -> u16 {
    match category {
        ProposalCategory::ProtocolUpgrade | ProposalCategory::Constitutional => {
            config.quorum_upgrade_bps
        }
        _ => config.quorum_bps,
    }
}

/// OFS-4100 §5: simple majority for Informational/Standards/Parameter,
/// a higher bar for Treasury, and the highest for Protocol-Upgrade/
/// Constitutional.
pub fn threshold_bps_for(config: &GovernanceConfig, category: ProposalCategory) -> u16 {
    match category {
        ProposalCategory::Treasury => config.threshold_treasury_bps,
        ProposalCategory::ProtocolUpgrade | ProposalCategory::Constitutional => {
            config.threshold_upgrade_bps
        }
        _ => config.threshold_simple_bps,
    }
}
