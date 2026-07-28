//! Wire-level constants. Event names follow OFS-2300 §21 exactly where it
//! names one (`PaymentSubmitted`, `PaymentReversed`, `SettlementApproved`,
//! `SettlementRejected`); `SettlementInitiated` and `SettlementCancelled`
//! aren't in §21's list at all, so those two fall back to OFS-8100's SET
//! namespace, which does define them.

use std::time::Duration;

pub const OFS_SPEC: u16 = 2300;

pub const EVENT_INITIATED: &str = "SettlementInitiated";
pub const EVENT_PAYMENT_SUBMITTED: &str = "PaymentSubmitted";
pub const EVENT_PAYMENT_REVERSED: &str = "PaymentReversed";
pub const EVENT_APPROVED: &str = "SettlementApproved";
pub const EVENT_REJECTED: &str = "SettlementRejected";
pub const EVENT_CANCELLED: &str = "SettlementCancelled";

/// §8a: Escrow Locked → Payment Sent.
pub const PAYMENT_WINDOW: Duration = Duration::from_secs(30 * 60);
/// §8a: Payment Sent → Approved/Rejected.
pub const MERCHANT_REVIEW_WINDOW: Duration = Duration::from_secs(30 * 60);
