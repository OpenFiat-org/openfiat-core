//! Pure, synchronous transaction-bytes validation shared by
//! [`crate::client::RpcChainClient`] (which decodes to actually submit)
//! and [`crate::state::ChainState`] (which only needs to reject a
//! malformed payload before ever queuing it — OFS-4300 §7).

use crate::error::ChainError;
use solana_transaction::versioned::VersionedTransaction;

pub fn validate_transaction_bytes(bytes: &[u8]) -> Result<(), ChainError> {
    bincode::deserialize::<VersionedTransaction>(bytes)
        .map(|_| ())
        .map_err(|_| ChainError::MalformedTransaction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_bytes() {
        assert_eq!(
            validate_transaction_bytes(&[]),
            Err(ChainError::MalformedTransaction)
        );
    }

    #[test]
    fn rejects_garbage_bytes() {
        assert_eq!(
            validate_transaction_bytes(&[1, 2, 3, 4, 5]),
            Err(ChainError::MalformedTransaction)
        );
    }
}
