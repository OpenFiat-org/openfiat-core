//! The connection lifecycle state machine (OFNP §8).
//!
//! ```text
//! Disconnected -> TransportConnected -> NoiseHandshake -> IdentityVerification
//!   -> ProtocolNegotiation -> ServiceNegotiation -> ActiveSession -> GracefulDisconnect
//! ```
//!
//! "Connections SHALL NOT exchange application messages before negotiation
//! completes successfully" — [`ConnectionState::may_exchange_application_messages`]
//! is the single choke point enforcing that.

/// A connection's position in the OFNP §8 lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    TransportConnected,
    NoiseHandshake,
    IdentityVerification,
    ProtocolNegotiation,
    ServiceNegotiation,
    ActiveSession,
    GracefulDisconnect,
}

impl ConnectionState {
    /// The state this one advances to on successful completion of its
    /// step, or `None` if there's nowhere further to advance (a terminal
    /// state).
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Disconnected => Some(Self::TransportConnected),
            Self::TransportConnected => Some(Self::NoiseHandshake),
            Self::NoiseHandshake => Some(Self::IdentityVerification),
            Self::IdentityVerification => Some(Self::ProtocolNegotiation),
            Self::ProtocolNegotiation => Some(Self::ServiceNegotiation),
            Self::ServiceNegotiation => Some(Self::ActiveSession),
            Self::ActiveSession => Some(Self::GracefulDisconnect),
            Self::GracefulDisconnect => None,
        }
    }

    /// Whether application messages may be exchanged in this state.
    ///
    /// Only true in `ActiveSession` — every earlier state is still
    /// negotiating, and `GracefulDisconnect` is winding down.
    pub const fn may_exchange_application_messages(self) -> bool {
        matches!(self, Self::ActiveSession)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_active_session_allows_application_messages() {
        assert!(!ConnectionState::Disconnected.may_exchange_application_messages());
        assert!(!ConnectionState::ProtocolNegotiation.may_exchange_application_messages());
        assert!(ConnectionState::ActiveSession.may_exchange_application_messages());
        assert!(!ConnectionState::GracefulDisconnect.may_exchange_application_messages());
    }

    #[test]
    fn walks_the_full_lifecycle_in_order() {
        let mut state = ConnectionState::Disconnected;
        let mut visited = vec![state];
        while let Some(next) = state.next() {
            state = next;
            visited.push(state);
        }
        assert_eq!(
            visited,
            vec![
                ConnectionState::Disconnected,
                ConnectionState::TransportConnected,
                ConnectionState::NoiseHandshake,
                ConnectionState::IdentityVerification,
                ConnectionState::ProtocolNegotiation,
                ConnectionState::ServiceNegotiation,
                ConnectionState::ActiveSession,
                ConnectionState::GracefulDisconnect,
            ]
        );
    }
}
