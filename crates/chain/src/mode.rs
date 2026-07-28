//! A node's connectivity to the Solana chain (OFS-4300 §4).

/// Which of the two connectivity modes OFS-4300 §4 defines a node is in.
///
/// `GossipOnly` is a real, documented capability boundary — it can supply
/// a blockhash (from gossip) and request transaction relay, but cannot
/// answer a query that needs a live account read, since it has no RPC
/// connection to make one with.
#[derive(Debug, Clone)]
pub enum NodeChainMode {
    RpcConnected {
        /// Submissions broadcast to every configured endpoint (OFS-4300
        /// §7 — multiple relay is expected, not an error).
        rpc_urls: Vec<String>,
        ws_url: Option<String>,
    },
    GossipOnly,
}

impl NodeChainMode {
    pub const fn is_rpc_connected(&self) -> bool {
        matches!(self, Self::RpcConnected { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_connected_reports_itself_as_such() {
        let mode = NodeChainMode::RpcConnected {
            rpc_urls: vec!["http://localhost:8899".into()],
            ws_url: None,
        };
        assert!(mode.is_rpc_connected());
    }

    #[test]
    fn gossip_only_reports_itself_as_such() {
        assert!(!NodeChainMode::GossipOnly.is_rpc_connected());
    }
}
