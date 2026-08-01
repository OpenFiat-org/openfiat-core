//! `openfiat-taxonomy` — what a merchant can say they will be paid with.
//!
//! Two halves, one shape.
//!
//! [`catalog`] is the rails this build ships and the countries each is
//! suggested in: a merchant in Kenya is offered M-Pesa, one in Brazil Pix,
//! one in Germany SEPA. It is a hand-written table compiled into the node,
//! served over one RPC method, and read by every client — which is the
//! whole of the improvement over each interface shipping its own copy, and
//! is worth stating plainly rather than dressing up. It is a suggestion
//! list and never a validation gate; see that module for the argument.
//!
//! [`store`] is the rails merchants define for themselves, as signed
//! records that replicate by gossip and are readable on every node. That
//! last part is the point: an "added" method that lives in one browser's
//! `localStorage` is invisible to the counterparty who has to pay it, and
//! calling it registered — as one picker's footnote did — is worse than
//! not offering it.
//!
//! # The three decisions this crate makes
//!
//! **An advertisement references a method by id, never by name.** A name
//! that can be edited after advertisements reference it is a rug-pull
//! vector, so there is no editable name: a built-in id is a stable column
//! of the catalog, and a merchant-defined id is a digest of the definition
//! itself. See [`PaymentMethodRef`].
//!
//! **A merchant-defined method is scoped to its author.** Globally
//! readable, so a counterparty on another node can resolve what an
//! advertisement means; selectable only by the merchant who defined it, so
//! there is no global namespace of arbitrary merchant text to squat. See
//! [`PaymentMethodRef::is_selectable_by`].
//!
//! **A name is checked at publication, not at render.** Control
//! characters, bidirectional overrides and invisible characters are
//! refused outright — the rule `openfiat_reviews` applies to comment text
//! — and a name that folds to the same skeleton as a rail this build
//! already ships is refused as a look-alike. See [`name`].

pub mod catalog;
pub mod error;
pub mod events;
pub mod name;
pub mod protocol;
pub mod record;
pub mod store;

#[cfg(test)]
mod fixtures;

pub use catalog::{catalog, for_country};
pub use error::TaxonomyError;
pub use events::SignedPaymentMethodDefine;
pub use name::{MAX_NAME_CHARS, check_name, skeleton};
pub use record::{
    BUILTIN, MerchantPaymentMethod, PaymentMethod, PaymentMethodCategory, PaymentMethodRef,
};
pub use store::{COLUMN_FAMILY as PAYMENT_METHODS_COLUMN_FAMILY, PaymentMethodRegistry};

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }
}
