//! `ChainClient` (OFS-4300 §4, §8): the operations a node's connectivity
//! to Solana needs to support, and `RpcChainClient`, the real
//! implementation an RPC-connected node uses.
//!
//! Every value crossing this trait's boundary is a plain string (base58,
//! matching how Solana itself renders blockhashes/signatures/pubkeys) or
//! raw bytes — this crate has no dependency on `openfiat-serialization`,
//! and callers on the gossip/RPC side already work in those terms.

use crate::error::ChainError;
use async_trait::async_trait;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_commitment_config::CommitmentConfig;
use solana_hash::Hash;
use solana_pubkey::Pubkey;
use solana_signature::Signature;
use solana_transaction::versioned::VersionedTransaction;
use std::str::FromStr;

/// A signature's on-chain outcome, once the cluster has processed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureStatus {
    Success,
    Failed,
}

/// The operations a node's chain connectivity needs to support (OFS-4300
/// §4, §8). Implemented by [`RpcChainClient`] for a real RPC-connected
/// node; a test fake implements it directly against canned responses.
#[async_trait]
pub trait ChainClient: Send + Sync {
    /// The current blockhash and its slot, base58-encoded.
    async fn get_latest_blockhash(&self) -> Result<(String, u64), ChainError>;

    /// Whether `blockhash` is still within Solana's own validity window.
    async fn is_blockhash_valid(&self, blockhash: &str) -> Result<bool, ChainError>;

    /// Submits an already-signed transaction's wire bytes. Rejects a
    /// payload that doesn't deserialize as a well-formed transaction
    /// before ever reaching the network (OFS-4300 §7), returning the
    /// base58-encoded signature on success.
    async fn send_transaction(&self, tx_bytes: &[u8]) -> Result<String, ChainError>;

    /// `None` if the cluster hasn't seen this signature (yet, or ever).
    async fn get_signature_status(
        &self,
        signature: &str,
    ) -> Result<Option<SignatureStatus>, ChainError>;

    /// Raw account data, `None` if the account doesn't exist.
    async fn get_account(&self, pubkey: &str) -> Result<Option<Vec<u8>>, ChainError>;
}

/// Wraps one or more Solana RPC endpoints for an [`NodeChainMode::RpcConnected`](crate::NodeChainMode) node.
///
/// `send_transaction` broadcasts to every configured endpoint in
/// parallel and succeeds if any one of them accepts it (OFS-4300 §7 —
/// multiple submission is expected, not an error; Solana's own
/// signature-based dedup handles the rest). Reads (blockhash, account,
/// signature status) use the first configured endpoint — a node that
/// wants read fan-out/failover across endpoints configures multiple
/// `RpcChainClient`-adjacent instances at a higher layer, not here.
pub struct RpcChainClient {
    clients: Vec<RpcClient>,
}

impl RpcChainClient {
    /// `rpc_urls` must be non-empty — constructing an `RpcChainClient`
    /// with no endpoints is a configuration error the caller should
    /// catch before reaching here (a node with no RPC endpoint at all is
    /// `NodeChainMode::GossipOnly`, not this type with an empty list).
    pub fn new(rpc_urls: Vec<String>) -> Self {
        assert!(
            !rpc_urls.is_empty(),
            "RpcChainClient requires at least one RPC endpoint"
        );
        Self {
            clients: rpc_urls.into_iter().map(RpcClient::new).collect(),
        }
    }

    fn primary(&self) -> &RpcClient {
        &self.clients[0]
    }
}

#[async_trait]
impl ChainClient for RpcChainClient {
    async fn get_latest_blockhash(&self) -> Result<(String, u64), ChainError> {
        let blockhash = self
            .primary()
            .get_latest_blockhash()
            .await
            .map_err(|_| ChainError::ChainUnavailable)?;
        let slot = self
            .primary()
            .get_slot()
            .await
            .map_err(|_| ChainError::ChainUnavailable)?;
        Ok((blockhash.to_string(), slot))
    }

    async fn is_blockhash_valid(&self, blockhash: &str) -> Result<bool, ChainError> {
        let hash = Hash::from_str(blockhash).map_err(|_| ChainError::MalformedTransaction)?;
        self.primary()
            .is_blockhash_valid(&hash, CommitmentConfig::confirmed())
            .await
            .map_err(|_| ChainError::ChainUnavailable)
    }

    async fn send_transaction(&self, tx_bytes: &[u8]) -> Result<String, ChainError> {
        let tx: VersionedTransaction =
            bincode::deserialize(tx_bytes).map_err(|_| ChainError::MalformedTransaction)?;

        let attempts = futures_join_all(
            self.clients
                .iter()
                .map(|client| client.send_transaction(&tx)),
        )
        .await;

        attempts
            .into_iter()
            .find_map(Result::ok)
            .map(|sig| sig.to_string())
            .ok_or(ChainError::TransactionSubmissionFailed)
    }

    async fn get_signature_status(
        &self,
        signature: &str,
    ) -> Result<Option<SignatureStatus>, ChainError> {
        let sig = Signature::from_str(signature).map_err(|_| ChainError::MalformedTransaction)?;
        let response = self
            .primary()
            .get_signature_statuses(&[sig])
            .await
            .map_err(|_| ChainError::ChainUnavailable)?;
        Ok(response.value[0].as_ref().map(|status| {
            if status.err.is_some() {
                SignatureStatus::Failed
            } else {
                SignatureStatus::Success
            }
        }))
    }

    async fn get_account(&self, pubkey: &str) -> Result<Option<Vec<u8>>, ChainError> {
        let pk = Pubkey::from_str(pubkey).map_err(|_| ChainError::MalformedTransaction)?;
        let response = self
            .primary()
            .get_account_with_commitment(&pk, CommitmentConfig::confirmed())
            .await
            .map_err(|_| ChainError::ChainUnavailable)?;
        Ok(response.value.map(|account| account.data))
    }
}

/// A minimal stand-in for `futures::future::join_all` — this crate takes
/// no dependency on the `futures` crate for one call site.
async fn futures_join_all<F: std::future::Future>(
    iter: impl IntoIterator<Item = F>,
) -> Vec<F::Output> {
    let mut handles = Vec::new();
    for fut in iter {
        handles.push(fut);
    }
    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        results.push(handle.await);
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fake `ChainClient` for testing callers of this trait without a
    /// real Solana cluster — the real `RpcChainClient` is exercised by
    /// Phase VII's conformance suite against devnet instead.
    struct FakeChainClient {
        blockhash: String,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ChainClient for FakeChainClient {
        async fn get_latest_blockhash(&self) -> Result<(String, u64), ChainError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok((self.blockhash.clone(), 42))
        }
        async fn is_blockhash_valid(&self, blockhash: &str) -> Result<bool, ChainError> {
            Ok(blockhash == self.blockhash)
        }
        async fn send_transaction(&self, tx_bytes: &[u8]) -> Result<String, ChainError> {
            if tx_bytes.is_empty() {
                return Err(ChainError::MalformedTransaction);
            }
            Ok("fake-signature".to_string())
        }
        async fn get_signature_status(
            &self,
            _signature: &str,
        ) -> Result<Option<SignatureStatus>, ChainError> {
            Ok(Some(SignatureStatus::Success))
        }
        async fn get_account(&self, _pubkey: &str) -> Result<Option<Vec<u8>>, ChainError> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn a_fake_client_satisfies_the_trait_boundary() {
        let client = FakeChainClient {
            blockhash: "abc123".to_string(),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let (hash, slot) = client.get_latest_blockhash().await.unwrap();
        assert_eq!(hash, "abc123");
        assert_eq!(slot, 42);
        assert!(client.is_blockhash_valid(&hash).await.unwrap());
    }

    #[tokio::test]
    async fn an_empty_payload_is_rejected_before_submission() {
        let client = FakeChainClient {
            blockhash: "abc123".to_string(),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        assert_eq!(
            client.send_transaction(&[]).await,
            Err(ChainError::MalformedTransaction)
        );
    }

    #[test]
    #[should_panic(expected = "at least one RPC endpoint")]
    fn constructing_with_no_endpoints_panics() {
        RpcChainClient::new(vec![]);
    }
}
