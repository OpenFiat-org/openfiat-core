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
        // A sealed payload that did not open — `openfiat_crypto`'s
        // `seal`, and every domain that carries a sealed payload through
        // it. Its own code rather than `InvalidSignature` (1003), where
        // it used to land: nothing about a signature is in question. The
        // envelope's signature verified, and what failed afterwards was
        // opening a box with a key that does not fit it. A peer told 1003
        // re-signs and re-sends the same undecryptable bytes.
        //
        // Says only that the box did not open. "Wrong key", "tampered
        // ciphertext" and "payload lifted from another slot" stay
        // collapsed into one code for the reason `SealError` collapses
        // them into one variant: telling them apart is an oracle.
        //
        // General rather than domain-ranged because no domain owns it —
        // it is raised by the crypto layer on behalf of whoever called
        // it, and OFS-8000's general range is where conditions with no
        // single owner live.
        //
        // Not retryable: the same ciphertext and the same key fail the
        // same way for as long as both are unchanged.
        DecryptionFailed = 10, "DECRYPTION_FAILED", false;
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
        // A session that was revoked, which is not a session that
        // expired. Both used to arrive as `SessionExpired` (1006), and
        // they call for opposite responses: expiry is the ordinary end of
        // a session's life and renewal is the remedy, while revocation is
        // deliberate and permanent (OFS-1400 §16) and no renewal undoes
        // it. A client told 1006 renews and is refused again; a client
        // told 1014 knows to establish a new session, and an operator
        // knows to ask who revoked this one.
        SessionRevoked = 1014, "SESSION_REVOKED", false;
        // A signed artifact whose own validity window has passed: a
        // wallet request outside its freshness window, a fee-settlement
        // quote past `MAX_QUOTE_VALIDITY`. Both were `SessionExpired`
        // (1006), which was the wrong instruction twice over — there is
        // no session anywhere in either path, and a client that responds
        // by re-authenticating has not touched the thing that expired.
        //
        // The remedy is to build the artifact again from current values
        // and sign it again: a fresh timestamp, or a fresh quote at the
        // current rate. Not retryable, because the stale value is inside
        // the bytes that were signed — the identical request carries the
        // identical expired window.
        RequestExpired = 1015, "REQUEST_EXPIRED", false;
    }
    range "Identity (2000-2999)" {
        IdentityNotFound = 2000, "IDENTITY_NOT_FOUND", false;
        InvalidIdentityClaim = 2001, "INVALID_IDENTITY_CLAIM", false;
        IdentityAlreadyExists = 2002, "IDENTITY_ALREADY_EXISTS", false;
        IdentityRevoked = 2003, "IDENTITY_REVOKED", false;
        ClaimVerificationFailed = 2004, "CLAIM_VERIFICATION_FAILED", false;
        InvalidSignatureChain = 2005, "INVALID_SIGNATURE_CHAIN", false;
        // An event signed by a node's own key that this node did not
        // emit: proof that a second process holds the same identity.
        //
        // Its own code rather than `InvalidSignature` (1003), where it
        // used to land. The signature verified — that is the entire
        // finding, and the code that says otherwise sends everyone who
        // reads it to the wrong place. A peer told 1003 re-signs; an
        // operator told 1003 audits their signing path. The actual
        // remedy is to stop running a copied `wallet.json` and rotate
        // the key, which no signature-shaped code will ever suggest.
        //
        // Not retryable: the duplicate holds the key until a human takes
        // it away.
        IdentityInUseElsewhere = 2006, "IDENTITY_IN_USE_ELSEWHERE", false;
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
        // "A settlement with this id is already on this node", which is
        // the one thing `SettlementAlreadyCompleted` (5005) does not say.
        // That is where a duplicate id used to land, and it is a
        // different statement about the trade: 5005 says the settlement
        // finished, while a rejected id says only that the id is taken —
        // by a settlement that may be sitting at `AwaitingPayment`, or
        // belong to two other people entirely.
        //
        // The path is the ordinary one. A client whose connection drops
        // mid-`sendSettlementInitiate` re-sends it, and was told its
        // trade had completed; a client that believes 5005 stops waiting
        // for a payment it should still be expecting.
        //
        // Its own code rather than the generic `ResourceAlreadyExists`
        // (7) because settlement has a range of its own. The generic is
        // the right answer for the domains OFS-8000 allocated no range to
        // — sessions, registry, risk, snapshot, content all use it and
        // say so — and the wrong one here, where every neighbouring
        // domain that does have a range names its own
        // (`ReservationAlreadyExists` 4001, `IdentityAlreadyExists` 2002,
        // `DisputeAlreadyOpen` 6001, `DuplicateAdvertisement` 3004).
        SettlementAlreadyExists = 5010, "SETTLEMENT_ALREADY_EXISTS", false;
    }
    range "Disputes (6000-6999)" {
        DisputeNotFound = 6000, "DISPUTE_NOT_FOUND", false;
        DisputeAlreadyOpen = 6001, "DISPUTE_ALREADY_OPEN", false;
        DisputeClosed = 6002, "DISPUTE_CLOSED", false;
        InvalidEvidence = 6003, "INVALID_EVIDENCE", false;
        DisputeTimeout = 6004, "DISPUTE_TIMEOUT", false;
        // The analogue of `InvalidReservationState` (4006) and
        // `InvalidSettlementState` (5009) that this range never had —
        // which is why an action illegal from the dispute's current
        // state had nowhere to land but `DisputeClosed` (6002).
        //
        // 6002 says the case is over. Almost nothing that reached it
        // was: an arbitrator joining a panel that has just filled, a
        // vote committed before the case locks, a reveal before the
        // commit phase ends. Every one of those is a live dispute, and a
        // participant told the dispute is closed stops acting on a case
        // they are still entitled — and sometimes obliged — to act on.
        //
        // 6002 keeps its number and its meaning and is mapped from
        // nothing today, the position 5005 holds. It is the right answer
        // for a case that genuinely resolved, and `openfiat-disputes`
        // does not yet separate that from the other illegal transitions;
        // the wrong answer is to keep calling the others by its name.
        //
        // Not retryable: a dispute's state moves when someone acts on
        // it, never because the same request arrived twice.
        InvalidDisputeState = 6005, "INVALID_DISPUTE_STATE", false;
    }
    range "Governance (7000-7999)" {
        ProposalNotFound = 7000, "PROPOSAL_NOT_FOUND", false;
        VotingClosed = 7001, "VOTING_CLOSED", false;
        DuplicateVote = 7002, "DUPLICATE_VOTE", false;
        InsufficientVotingPower = 7003, "INSUFFICIENT_VOTING_POWER", false;
        InvalidProposal = 7004, "INVALID_PROPOSAL", false;
        // "That proposal id is already taken", which is the one thing
        // `InvalidProposal` (7004) does not say. A duplicate id used to
        // land there, and 7004 is a verdict on a proposal's *content*:
        // an author told their proposal is invalid rewrites text that
        // was never the problem, when the only thing wrong is a
        // collision — with a stranger's proposal, or with their own
        // resend after a dropped connection.
        //
        // Its own code rather than the generic `ResourceAlreadyExists`
        // (7), for the reason `SettlementAlreadyExists` (5010) gives:
        // the generic belongs to the domains OFS-8000 allocated no range
        // to, and governance has one.
        ProposalAlreadyExists = 7005, "PROPOSAL_ALREADY_EXISTS", false;
        // Withdrawing a proposal that already executed, activating one
        // already active, voting on a status that does not accept votes.
        // The state analogue this range lacked, and the second condition
        // 7004 was absorbing: "you cannot do that from here" is not
        // "your proposal is invalid", and only one of the two is fixed
        // by editing the proposal.
        //
        // Not retryable, for the reason 5009 is not: the transition is
        // illegal for as long as the proposal stays where it is, and
        // nothing about resending changes where it is.
        InvalidProposalState = 7006, "INVALID_PROPOSAL_STATE", false;
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
