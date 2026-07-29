//! Notification failures, mapped onto OFS-8000's Notifications range
//! (8000-8999) where a code exists there, and the closest applicable
//! code otherwise.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum NotificationError {
    InvalidSignature,
    /// A delivery receipt signed by someone other than the referenced
    /// service's on-file provider (per `openfiat-registry`).
    Unauthorized,
    MalformedEvent,
    ServiceNotFound,
    SubscriptionNotFound,
    /// A delivery report referencing a notification this node never
    /// dispatched — see `NotificationRegistry::apply_delivery_report`
    /// for why an unverifiable report is dropped rather than trusted.
    UnknownNotification,
    /// §5's plugin `NotificationProvider::send` failing because the
    /// provider itself couldn't be reached (transient).
    ProviderUnavailable,
    /// §5's plugin `NotificationProvider::send` failing for a concrete
    /// reason a caller might want to display.
    DeliveryFailed(String),
    InvalidDestination,
    UnsupportedChannel,
}

impl NotificationError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::Unauthorized => ErrorCode::InvalidRequest,
            Self::MalformedEvent => ErrorCode::DeserializationError,
            Self::ServiceNotFound => ErrorCode::ResourceNotFound,
            Self::UnknownNotification => ErrorCode::ResourceNotFound,
            Self::SubscriptionNotFound => ErrorCode::SubscriptionNotFound,
            Self::ProviderUnavailable => ErrorCode::NotificationProviderUnavailable,
            Self::DeliveryFailed(_) => ErrorCode::DeliveryFailed,
            Self::InvalidDestination => ErrorCode::InvalidDestination,
            Self::UnsupportedChannel => ErrorCode::UnsupportedNotificationType,
        }
    }
}

impl fmt::Display for NotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for NotificationError {}
