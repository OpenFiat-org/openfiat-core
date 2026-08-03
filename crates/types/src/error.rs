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
            /// Every code in the registry, in declaration order.
            ///
            /// Exists so the registry can be checked *as a whole* rather
            /// than one spot-checked entry at a time — that a number is
            /// never reused, that a name is never reused, and that
            /// `retryable` is only ever true where somebody said so out
            /// loud. See this module's tests. A hand-written list would
            /// drift from the macro the first time a code was added
            /// without it; this one cannot.
            pub const ALL: &'static [ErrorCode] = &[ $( $( Self::$variant, )+ )+ ];

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
        ChainUnavailable = 1010, "CHAIN_UNAVAILABLE", true;
        BlockhashExpired = 1011, "BLOCKHASH_EXPIRED", true;
        MalformedTransaction = 1012, "MALFORMED_TRANSACTION", false;
        TransactionSubmissionFailed = 1013, "TRANSACTION_SUBMISSION_FAILED", true;
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
        // One merchant has reached `openfiat_taxonomy::store::
        // MAX_METHODS_PER_MERCHANT` and this definition displaces none of
        // them. Its own code rather than `RateLimitExceeded`, which is
        // where it used to land: a rate limit is a speed, and every client
        // that handles one handles it by waiting and trying again. This
        // cap is a count. It does not decay, nothing frees a slot but the
        // merchant retiring a definition, and a caller told to back off
        // will back off forever.
        PaymentMethodLimitReached = 3006, "PAYMENT_METHOD_LIMIT_REACHED", false;
    }
    range "Reservation & Marketplace (4000-4999)" {
        ReservationNotFound = 4000, "RESERVATION_NOT_FOUND", false;
        ReservationAlreadyExists = 4001, "RESERVATION_ALREADY_EXISTS", false;
        ReservationExpired = 4002, "RESERVATION_EXPIRED", false;
        ReservationCancelled = 4003, "RESERVATION_CANCELLED", false;
        InsufficientAvailableLiquidity = 4004, "INSUFFICIENT_AVAILABLE_LIQUIDITY", true;
        MerchantOffline = 4005, "MERCHANT_OFFLINE", true;
        InvalidReservationState = 4006, "INVALID_RESERVATION_STATE", false;
        // The price the requester signed is not one the advertisement's own
        // terms produce. Its own code rather than the generic
        // `InvalidRequest` it used to share with every other malformed
        // field: a taker whose price disagreed could not tell that from a
        // typo in their amount, and the two call for opposite responses —
        // re-read the book and sign again, versus fix the request.
        // Retryable, because the honest cause is a quote that moved
        // between reading it and signing it.
        PriceDisagreement = 4007, "PRICE_DISAGREEMENT", true;
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
        // The pair the reservation range has had since it was written
        // (`ReservationNotFound` 4000 / `InvalidReservationState` 4006),
        // arriving here late and for a reason: both of these used to be
        // `SettlementFailed`, which is retryable.
        //
        // That was survivable while the only settlement mutations were
        // `sendPaymentSubmitted` and `sendSettlementApproved`. It stopped
        // being survivable when cancellation, rejection and payment
        // reversal became reachable, because those are the calls a client
        // makes speculatively — "can I still cancel this?" — and the
        // answer "no, it is too late" was arriving as a retryable code
        // with the same name and number as "no such settlement". A client
        // could neither tell the two apart nor learn to stop asking.
        SettlementNotFound = 5008, "SETTLEMENT_NOT_FOUND", false;
        InvalidSettlementState = 5009, "INVALID_SETTLEMENT_STATE", false;
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
    use std::collections::HashSet;

    /// Retrying is the exception, and every exception is named here.
    ///
    /// This list is the point of the test below it. `retryable` is a
    /// promise to every client in every language that the same request,
    /// sent again unchanged, may succeed — so a code that carries it
    /// wrongly does not produce a wrong error message, it produces a
    /// client that never stops asking.
    ///
    /// That is not hypothetical. `SettlementFailed` was `true` and was
    /// the code for *both* "no such settlement" and "that transition is
    /// illegal from this state". The second is permanent by definition,
    /// and once cancellation, rejection and payment reversal became
    /// reachable over RPC it became the answer to the most speculative
    /// question a client asks — "can I still cancel this?". Splitting
    /// those two out (5008/5009) fixed the instance. This list is what
    /// stops the next one, because adding a retryable code now means
    /// writing it down here, next to the reason retryability is load
    /// bearing.
    const RETRYABLE: &[ErrorCode] = &[
        // Transient by nature: the machine, the network, or the chain is
        // busy, unreachable, or behind, and none of that is a property of
        // the request.
        ErrorCode::InternalError,
        ErrorCode::OperationTimeout,
        ErrorCode::RateLimitExceeded,
        ErrorCode::NetworkError,
        ErrorCode::PeerNotFound,
        ErrorCode::NodeNotSynchronized,
        ErrorCode::NetworkUnavailable,
        ErrorCode::ChainUnavailable,
        ErrorCode::BlockhashExpired,
        ErrorCode::TransactionSubmissionFailed,
        ErrorCode::BlockchainConfirmationTimeout,
        ErrorCode::DatabaseError,
        // A resource that replenishes without anyone fixing anything: a
        // merchant's liquidity returns when a reservation expires, a
        // merchant comes back online, a quote moves back into range.
        ErrorCode::InsufficientAvailableLiquidity,
        ErrorCode::MerchantOffline,
        ErrorCode::PriceDisagreement,
        ErrorCode::VaultInsufficientBalance,
        // Delivery, where "again" is the entire remedy.
        ErrorCode::NotificationProviderUnavailable,
        ErrorCode::DeliveryFailed,
        // The one judgement call in this list, and `openfiat_snapshot`'s
        // own `code()` explains it at length: a snapshot that fails
        // verification is permanently bad, but the retry a bootstrapping
        // node performs is against a *different* snapshot from a
        // different provider, which is exactly the loop this flag drives.
        // That crate deliberately routes the permanent cases
        // (`InsufficientProviderStake`) to a non-retryable code instead.
        ErrorCode::SnapshotVerificationFailed,
        // Settlement's remaining generic failure. Nothing maps to it
        // today — `SettlementNotFound` and `InvalidSettlementState` took
        // over the two conditions that did — but it is OFS-8000's own
        // code and stays in the registry.
        ErrorCode::SettlementFailed,
    ];

    /// The check that turns "we thought about retryability" into
    /// something a future edit cannot quietly undo.
    ///
    /// Modelled on `openfiat_api::openrpc`'s
    /// `no_method_is_documented_by_accident`, for the same reason: a
    /// hand-maintained judgement that fails open is a judgement that
    /// stops being made. A new code defaults to non-retryable here, which
    /// is the safe direction — the worst case is a client that gives up
    /// on something it could have retried, rather than one that hammers a
    /// node over an outcome that will never change.
    #[test]
    fn no_code_is_retryable_by_accident() {
        let expected: HashSet<u16> = RETRYABLE.iter().map(|c| c.code()).collect();
        for code in ErrorCode::ALL {
            assert_eq!(
                code.retryable(),
                expected.contains(&code.code()),
                "{} ({}) disagrees with the RETRYABLE list in this module. If you have just \
                 marked it retryable, add it to that list and say why: `retryable` tells every \
                 client in every language that sending the same request again may work, so a \
                 permanent outcome wearing it produces a client that never stops asking. If you \
                 have just added it to the list, set the flag.",
                code.name(),
                code.code(),
            );
        }
    }

    /// Two variants sharing a number, or a name, would make the registry
    /// ambiguous on the wire — a client matching on `ofsErrorCode` would
    /// silently handle one condition as another. Nothing prevented it
    /// before; the macro cannot, and three codes were added to this file
    /// by hand in one sitting.
    #[test]
    fn every_code_and_name_appears_exactly_once() {
        let mut numbers = HashSet::new();
        let mut names = HashSet::new();
        for code in ErrorCode::ALL {
            assert!(
                numbers.insert(code.code()),
                "{} reuses numeric code {}",
                code.name(),
                code.code()
            );
            assert!(names.insert(code.name()), "duplicate name {}", code.name());
        }
        assert_eq!(numbers.len(), ErrorCode::ALL.len());
    }

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
