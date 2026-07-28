//! Bootstrap policy (OFS-1100 §5-6, §17).
//!
//! Bootstrap nodes only introduce new participants; they're never required
//! again once a node has enough healthy peers of its own ("Bootstrap
//! Independence", §17 — "If every official bootstrap node disappeared, the
//! network SHOULD continue operating indefinitely").

/// Whether a node with `healthy_peer_count` cached peers should still
/// bother contacting a bootstrap node, given it wants at least `minimum`.
///
/// "Whenever a local cache already exists, bootstrap nodes SHOULD only be
/// contacted if insufficient healthy peers remain" (§6).
pub fn should_contact_bootstrap(healthy_peer_count: usize, minimum: usize) -> bool {
    healthy_peer_count < minimum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contacts_bootstrap_when_below_the_minimum() {
        assert!(should_contact_bootstrap(0, 32));
        assert!(should_contact_bootstrap(31, 32));
    }

    #[test]
    fn skips_bootstrap_once_the_minimum_is_met() {
        assert!(!should_contact_bootstrap(32, 32));
        assert!(!should_contact_bootstrap(96, 32));
    }
}
