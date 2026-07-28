//! The canonical protocol error registry (OFS-8000).
//!
//! Every OpenFiat implementation — nodes, SDKs, wallets, merchant apps,
//! JSON-RPC, REST, WebSocket, CLI — reports failures using these same
//! numeric codes and symbolic names, so a client can react identically
//! regardless of which language or transport it's talking through.
//!
//! `retryable()` reflects OpenFiat-core's own judgment call for the codes
//! OFS-8000 §16 doesn't name explicitly (it gives four retryable and four
//! non-retryable examples out of ~70 codes): anything representing a
//! transient/environmental condition (timeouts, unavailability, exhausted
//! resources that can replenish) is retryable; anything representing a
//! logical outcome that won't change on blind retry (not-found, already-X,
//! invalid-X, expired/closed/cancelled) is not.

macro_rules! error_registry {
    ( $( range $range_doc:literal { $( $variant:ident = $code:expr, $name:literal, $retryable:expr );+ $(;)? } )+ ) => {
        /// A canonical OFS-8000 error code.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        pub enum ErrorCode {
            $( $( #[doc = $range_doc] $variant, )+ )+
        }

        impl ErrorCode {
            /// The numeric code (e.g. `4004` for `InsufficientAvailableLiquidity`).
            pub const fn code(self) -> u16 {
                match self {
                    $( $( Self::$variant => $code, )+ )+
                }
            }

            /// The stable symbolic identifier, exactly as OFS-8000 §5-15 name it
            /// (e.g. `"INSUFFICIENT_AVAILABLE_LIQUIDITY"`).
            pub const fn name(self) -> &'static str {
                match self {
                    $( $( Self::$variant => $name, )+ )+
                }
            }

            /// Whether a client MAY retry the request as-is and expect a
            /// different outcome (OFS-8000 §16).
            pub const fn retryable(self) -> bool {
                match self {
                    $( $( Self::$variant => $retryable, )+ )+
                }
            }
        }
    };
}

error_registry! {
    range "General Protocol (0000-0999)" {
        UnknownError = 0, "UNKNOWN_ERROR", false;
        InternalError = 1, "INTERNAL_ERROR", true;
        InvalidRequest = 2, "INVALID_REQUEST", false;
        InvalidParameter = 3, "INVALID_PARAMETER", false;
        UnsupportedOperation = 4, "UNSUPPORTED_OPERATION", false;
        NotImplemented = 5, "NOT_IMPLEMENTED", false;
        ResourceNotFound = 6, "RESOURCE_NOT_FOUND", false;
        ResourceAlreadyExists = 7, "RESOURCE_ALREADY_EXISTS", false;
        OperationTimeout = 8, "OPERATION_TIMEOUT", true;
        RateLimitExceeded = 9, "RATE_LIMIT_EXCEEDED", true;
    }
    range "Network (1000-1999)" {
        NetworkError = 1000, "NETWORK_ERROR", true;
        PeerNotFound = 1001, "PEER_NOT_FOUND", true;
        ProtocolVersionMismatch = 1002, "PROTOCOL_VERSION_MISMATCH", false;
        InvalidSignature = 1003, "INVALID_SIGNATURE", false;
        ReplayAttackDetected = 1004, "REPLAY_ATTACK_DETECTED", false;
        SnapshotVerificationFailed = 1005, "SNAPSHOT_VERIFICATION_FAILED", true;
        SessionExpired = 1006, "SESSION_EXPIRED", false;
        MessageOutOfOrder = 1007, "MESSAGE_OUT_OF_ORDER", false;
        NodeNotSynchronized = 1008, "NODE_NOT_SYNCHRONIZED", true;
        NetworkUnavailable = 1009, "NETWORK_UNAVAILABLE", true;
    }
    range "Identity (2000-2999)" {
        IdentityNotFound = 2000, "IDENTITY_NOT_FOUND", false;
        InvalidIdentityClaim = 2001, "INVALID_IDENTITY_CLAIM", false;
        IdentityAlreadyExists = 2002, "IDENTITY_ALREADY_EXISTS", false;
        IdentityRevoked = 2003, "IDENTITY_REVOKED", false;
        ClaimVerificationFailed = 2004, "CLAIM_VERIFICATION_FAILED", false;
        InvalidSignatureChain = 2005, "INVALID_SIGNATURE_CHAIN", false;
    }
    range "Advertisement (3000-3999)" {
        AdvertisementNotFound = 3000, "ADVERTISEMENT_NOT_FOUND", false;
        AdvertisementDisabled = 3001, "ADVERTISEMENT_DISABLED", false;
        AdvertisementExpired = 3002, "ADVERTISEMENT_EXPIRED", false;
        InvalidAdvertisement = 3003, "INVALID_ADVERTISEMENT", false;
        DuplicateAdvertisement = 3004, "DUPLICATE_ADVERTISEMENT", false;
        UnsupportedPaymentMethod = 3005, "UNSUPPORTED_PAYMENT_METHOD", false;
    }
    range "Reservation & Marketplace (4000-4999)" {
        ReservationNotFound = 4000, "RESERVATION_NOT_FOUND", false;
        ReservationAlreadyExists = 4001, "RESERVATION_ALREADY_EXISTS", false;
        ReservationExpired = 4002, "RESERVATION_EXPIRED", false;
        ReservationCancelled = 4003, "RESERVATION_CANCELLED", false;
        InsufficientAvailableLiquidity = 4004, "INSUFFICIENT_AVAILABLE_LIQUIDITY", true;
        MerchantOffline = 4005, "MERCHANT_OFFLINE", true;
        InvalidReservationState = 4006, "INVALID_RESERVATION_STATE", false;
    }
    range "Settlement & Liquidity (5000-5999)" {
        SettlementFailed = 5000, "SETTLEMENT_FAILED", true;
        VaultInsufficientBalance = 5001, "VAULT_INSUFFICIENT_BALANCE", true;
        InvalidDeposit = 5002, "INVALID_DEPOSIT", false;
        UnsupportedStablecoin = 5003, "UNSUPPORTED_STABLECOIN", false;
        BlockchainConfirmationTimeout = 5004, "BLOCKCHAIN_CONFIRMATION_TIMEOUT", true;
        SettlementAlreadyCompleted = 5005, "SETTLEMENT_ALREADY_COMPLETED", false;
        SettlementAlreadyCancelled = 5006, "SETTLEMENT_ALREADY_CANCELLED", false;
        FlaggedDepositAddress = 5007, "FLAGGED_DEPOSIT_ADDRESS", false;
    }
    range "Disputes (6000-6999)" {
        DisputeNotFound = 6000, "DISPUTE_NOT_FOUND", false;
        DisputeAlreadyOpen = 6001, "DISPUTE_ALREADY_OPEN", false;
        DisputeClosed = 6002, "DISPUTE_CLOSED", false;
        InvalidEvidence = 6003, "INVALID_EVIDENCE", false;
        DisputeTimeout = 6004, "DISPUTE_TIMEOUT", false;
    }
    range "Governance (7000-7999)" {
        ProposalNotFound = 7000, "PROPOSAL_NOT_FOUND", false;
        VotingClosed = 7001, "VOTING_CLOSED", false;
        DuplicateVote = 7002, "DUPLICATE_VOTE", false;
        InsufficientVotingPower = 7003, "INSUFFICIENT_VOTING_POWER", false;
        InvalidProposal = 7004, "INVALID_PROPOSAL", false;
    }
    range "Notifications (8000-8999)" {
        NotificationProviderUnavailable = 8000, "NOTIFICATION_PROVIDER_UNAVAILABLE", true;
        DeliveryFailed = 8001, "DELIVERY_FAILED", true;
        InvalidDestination = 8002, "INVALID_DESTINATION", false;
        UnsupportedNotificationType = 8003, "UNSUPPORTED_NOTIFICATION_TYPE", false;
        SubscriptionNotFound = 8004, "SUBSCRIPTION_NOT_FOUND", false;
    }
    range "Internal & Implementation (9000-9999)" {
        DatabaseError = 9000, "DATABASE_ERROR", true;
        StorageCorrupted = 9001, "STORAGE_CORRUPTED", false;
        ConfigurationError = 9002, "CONFIGURATION_ERROR", false;
        SerializationError = 9003, "SERIALIZATION_ERROR", false;
        DeserializationError = 9004, "DESERIALIZATION_ERROR", false;
        UnknownImplementationError = 9005, "UNKNOWN_IMPLEMENTATION_ERROR", false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_names_match_the_registry() {
        assert_eq!(ErrorCode::InsufficientAvailableLiquidity.code(), 4004);
        assert_eq!(
            ErrorCode::InsufficientAvailableLiquidity.name(),
            "INSUFFICIENT_AVAILABLE_LIQUIDITY"
        );
    }

    #[test]
    fn spec_named_examples_have_the_documented_retryability() {
        // OFS-8000 §16's explicit retryable examples.
        assert!(ErrorCode::NetworkUnavailable.retryable());
        assert!(ErrorCode::OperationTimeout.retryable());
        assert!(ErrorCode::DeliveryFailed.retryable());
        assert!(ErrorCode::BlockchainConfirmationTimeout.retryable());
        // OFS-8000 §16's explicit non-retryable examples.
        assert!(!ErrorCode::InvalidRequest.retryable());
        assert!(!ErrorCode::InvalidSignature.retryable());
        assert!(!ErrorCode::AdvertisementExpired.retryable());
        assert!(!ErrorCode::FlaggedDepositAddress.retryable());
    }

    #[test]
    fn every_code_falls_within_its_documented_range() {
        let ranges: &[(ErrorCode, u16, u16)] = &[
            (ErrorCode::UnknownError, 0, 999),
            (ErrorCode::NetworkError, 1000, 1999),
            (ErrorCode::IdentityNotFound, 2000, 2999),
            (ErrorCode::AdvertisementNotFound, 3000, 3999),
            (ErrorCode::ReservationNotFound, 4000, 4999),
            (ErrorCode::SettlementFailed, 5000, 5999),
            (ErrorCode::DisputeNotFound, 6000, 6999),
            (ErrorCode::ProposalNotFound, 7000, 7999),
            (ErrorCode::NotificationProviderUnavailable, 8000, 8999),
            (ErrorCode::DatabaseError, 9000, 9999),
        ];
        for (code, low, high) in ranges {
            assert!(
                (*low..=*high).contains(&code.code()),
                "{} out of range",
                code.name()
            );
        }
    }
}
