//! Gossip channels (OGP §18-19): "Nodes MAY selectively subscribe to
//! event categories... this reduces unnecessary bandwidth."

/// One of the logical channels events are separated into (§19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Channel {
    Marketplace,
    Governance,
    Reputation,
    Notifications,
    Oracle,
    RiskIntelligence,
    Chain,
    Infrastructure,
}

impl Channel {
    /// Which channel an event belongs to, from its OFS specification
    /// number. Falls back to `Infrastructure` for specs not yet mapped
    /// (the network/discovery/registry/session layer's own housekeeping
    /// events) rather than refusing to classify them.
    pub const fn for_ofs_spec(ofs_spec: u16) -> Channel {
        match ofs_spec {
            2000..=2999 => Channel::Marketplace,
            4000 => Channel::Governance,
            3000 => Channel::Reputation,
            6000 => Channel::Notifications,
            7000 => Channel::Oracle,
            7100 => Channel::RiskIntelligence,
            4300 => Channel::Chain,
            _ => Channel::Infrastructure,
        }
    }
}

/// The set of channels a node is subscribed to. `All` is the common case
/// for a full node; a service-specific node (e.g. a bare Oracle Provider)
/// can subscribe to just what it needs (§18).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Subscription {
    All,
    Only(Vec<Channel>),
}

impl Subscription {
    pub fn accepts(&self, channel: Channel) -> bool {
        match self {
            Subscription::All => true,
            Subscription::Only(channels) => channels.contains(&channel),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_ofs_ranges_to_their_channel() {
        assert_eq!(Channel::for_ofs_spec(2100), Channel::Marketplace);
        assert_eq!(Channel::for_ofs_spec(4000), Channel::Governance);
        assert_eq!(Channel::for_ofs_spec(7000), Channel::Oracle);
        assert_eq!(Channel::for_ofs_spec(7100), Channel::RiskIntelligence);
        assert_eq!(Channel::for_ofs_spec(4300), Channel::Chain);
        assert_eq!(Channel::for_ofs_spec(1000), Channel::Infrastructure);
    }

    #[test]
    fn subscription_all_accepts_everything() {
        assert!(Subscription::All.accepts(Channel::Governance));
    }

    #[test]
    fn subscription_only_accepts_just_the_listed_channels() {
        let sub = Subscription::Only(vec![Channel::Oracle]);
        assert!(sub.accepts(Channel::Oracle));
        assert!(!sub.accepts(Channel::Governance));
    }
}
