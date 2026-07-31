//! What a service says it is called and looks like (OFS-1500 §9 —
//! "every service advertises metadata describing its capabilities").
//!
//! Until this existed a registry entry was a Service ID, a peer id and a
//! list of endpoints, so every provider in a directory was an anonymous
//! hex string. That is a fair description of what the protocol *knows*
//! and a poor one of what an operator is: somebody runs that node, they
//! have a name for it, and a client choosing between two of them has
//! nothing to choose on.
//!
//! # Every field here is self-asserted
//!
//! A registration is signed by the key that made it and by nobody else.
//! Signing proves the record was not altered in transit; it proves
//! nothing about whether the name is the signer's to use. Anyone can
//! register a service called "Binance". So this module's job is to bound
//! what a provider can put on every other node's disk and to refuse the
//! values that would misrender or mislead — not to adjudicate identity,
//! which it cannot do. Consumers must present all of it as a claim; see
//! `openfiat-app`'s provider views, which label it as declared.

use crate::error::RegistryError;

/// Longest accepted [`ServiceBranding::name`], in characters.
///
/// # Why any bound at all
///
/// A registration is gossiped to every node on the network and each one
/// stores it in RocksDB until the service withdraws or expires. An
/// unbounded string is therefore not "a long name" — it is a write
/// primitive into several hundred strangers' disks, at the cost of one
/// signature, repeated for free on every re-registration. The numbers
/// below are what a directory row and a detail paragraph can actually
/// show; anything longer would be truncated by the reader anyway, so the
/// only thing an unbounded field buys is the abuse.
pub const MAX_NAME_CHARS: usize = 64;

/// Longest accepted [`ServiceBranding::description`], in characters —
/// a sentence or two, the length a provider detail page renders.
/// See [`MAX_NAME_CHARS`] for why the ceiling exists.
pub const MAX_DESCRIPTION_CHARS: usize = 280;

/// Longest accepted [`ServiceBranding::website`], in characters.
/// Generous next to real URLs and still a bound; see [`MAX_NAME_CHARS`].
pub const MAX_WEBSITE_CHARS: usize = 256;

/// Presentation a provider declares for one registered service.
///
/// Optional as a whole, and every field optional within it: a node on a
/// laptop has nothing to say here, and an absent name is a better answer
/// than an invented one.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServiceBranding {
    /// A human name for the service — "AllenHark EU", not a legal entity.
    ///
    /// Never a substitute for the Service ID or the provider peer id.
    /// Two providers may register the same name, deliberately: refusing
    /// a duplicate would make the registry first-come-first-served over
    /// a global namespace, which is a land grab and not a service
    /// registry. Consumers show the name *and* the id.
    pub name: Option<String>,

    /// A sentence about what this service is for.
    pub description: Option<String>,

    /// A logo, as an IPFS CID — the same image a client uses as the
    /// service's avatar where it would otherwise draw a placeholder from
    /// the peer id.
    ///
    /// # Why a CID and not a URL
    ///
    /// This decision is already made in this workspace, by
    /// `openfiat_identity::ClaimType::Avatar`, and it is made the same
    /// way here for the same reasons. Image bytes in the registration
    /// would put a logo into every node's gossip log and replay it
    /// forever. A URL would let the operator change what the picture is
    /// after publication, silently, for everyone — and, worse, would
    /// make every viewer of a provider directory issue a request to a
    /// server the provider controls. That is a tracking beacon: whoever
    /// hosts the image learns the IP, the time and the page of everyone
    /// who so much as scrolled past the row.
    ///
    /// A CID names one specific image and is served by the node the
    /// viewer already chose to talk to, over its own `GET /ipfs/{cid}`.
    /// Nobody new learns anything.
    pub logo: Option<String>,

    /// A website for whoever runs this service.
    ///
    /// Deliberately not called `url`: a registration already carries
    /// `endpoints`, which is where the *service* is, and an operator who
    /// confused the two would publish a homepage as a dialable endpoint
    /// or an API root as their marketing site. This is the one a reader
    /// clicks.
    pub website: Option<String>,
}

impl ServiceBranding {
    /// Whether anything was actually declared.
    ///
    /// A `Some(ServiceBranding)` with every field `None` is the same
    /// statement as `None`, and a consumer should not have to know both
    /// spellings of "said nothing".
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.description.is_none()
            && self.logo.is_none()
            && self.website.is_none()
    }

    /// Shape checks, run before a registration is stored or gossiped on.
    ///
    /// Called from [`crate::SignedRegistration::verify`] rather than
    /// from a constructor, so a value that this node would have refused
    /// cannot arrive from a peer instead — the same discipline
    /// `openfiat_identity` follows for avatar claims, and for the same
    /// reason: validating only at the point where a local operator types
    /// it leaves the gossip path unguarded, which is the path an
    /// attacker actually uses.
    pub fn validate(&self) -> Result<(), RegistryError> {
        check_text(self.name.as_deref(), MAX_NAME_CHARS)?;
        check_text(self.description.as_deref(), MAX_DESCRIPTION_CHARS)?;
        check_website(self.website.as_deref())?;
        if let Some(logo) = &self.logo {
            // Checked here, not at render time, so a string that is not
            // a CID never reaches storage, never reaches gossip, and
            // never reaches a viewer that might concatenate it into a
            // gateway URL. `openfiat_identity::ClaimType::accepts` makes
            // the same call for the same reason.
            openfiat_crypto::Cid::parse(logo).map_err(|_| RegistryError::MalformedBranding)?;
        }
        Ok(())
    }
}

/// A declared string that is present must be non-empty, within bounds,
/// and safe to render.
fn check_text(value: Option<&str>, max_chars: usize) -> Result<(), RegistryError> {
    let Some(value) = value else { return Ok(()) };
    // `Some("")` is not a declaration, it is a `None` spelled in a way
    // that makes a reader render an empty row. Refused so there is one
    // way to say nothing.
    if value.is_empty() || value.chars().count() > max_chars {
        return Err(RegistryError::MalformedBranding);
    }
    if value.chars().any(is_display_hostile) {
        return Err(RegistryError::MalformedBranding);
    }
    Ok(())
}

/// A website must be an ordinary web address a reader can click.
fn check_website(value: Option<&str>) -> Result<(), RegistryError> {
    let Some(value) = value else { return Ok(()) };
    check_text(Some(value), MAX_WEBSITE_CHARS)?;
    // Scheme allowlist, not a denylist. This string ends up in an
    // `href`, and `javascript:` there is script execution in the
    // viewer's page under the viewer's origin — the one input on this
    // record that turns a registration into code. Enumerating what is
    // refused would mean keeping up with `data:`, `vbscript:`, `blob:`
    // and whatever a browser ships next; enumerating what is allowed
    // does not.
    let lower = value.to_ascii_lowercase();
    if !lower.starts_with("https://") && !lower.starts_with("http://") {
        return Err(RegistryError::MalformedBranding);
    }
    // A homepage in a domain reserved never to resolve is as fictional
    // as an endpoint in one, and the registry already refuses those —
    // see `registration::is_unresolvable` for the devnet incident that
    // rule exists because of.
    if crate::registration::is_unresolvable(value) {
        return Err(RegistryError::MalformedBranding);
    }
    Ok(())
}

/// Characters that make rendered text say something other than what was
/// signed.
///
/// Two families, both of which have been used against directories
/// before. C0/C1 controls let a name carry a newline or a NUL and break
/// out of the row it belongs in. The Unicode bidirectional overrides let
/// a name render right-to-left, so a signed `"…gro.tsurt"` can appear on
/// screen as `"trust.org…"` — the reader sees a string that is not the
/// one any of this was verified against.
///
/// Not a general "printable" filter: names in this network are Swahili,
/// Arabic and Chinese as often as they are ASCII, and a rule that
/// admitted only Latin letters would be a bug dressed as a safety
/// measure.
fn is_display_hostile(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real CIDv1, base32, sha2-256 — the shape `openfiat_crypto::Cid`
    /// accepts and the shape a node's `GET /ipfs/{cid}` can serve.
    const LOGO_CID: &str = "bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva";

    /// A declaration whose only content is a name.
    fn named(name: impl Into<String>) -> ServiceBranding {
        ServiceBranding {
            name: Some(name.into()),
            ..ServiceBranding::default()
        }
    }

    /// A declaration whose only content is a website.
    fn site(website: &str) -> ServiceBranding {
        ServiceBranding {
            website: Some(website.to_string()),
            ..ServiceBranding::default()
        }
    }

    fn branding() -> ServiceBranding {
        ServiceBranding {
            name: Some("AllenHark EU".to_string()),
            description: Some("Public API node in Frankfurt, run by AllenHark.".to_string()),
            logo: Some(LOGO_CID.to_string()),
            website: Some("https://openfiat.allenhark.com".to_string()),
        }
    }

    #[test]
    fn an_ordinary_declaration_is_accepted() {
        assert_eq!(branding().validate(), Ok(()));
    }

    #[test]
    fn declaring_nothing_is_accepted_and_reads_as_empty() {
        let none = ServiceBranding::default();
        assert_eq!(none.validate(), Ok(()));
        assert!(none.is_empty());
        assert!(!branding().is_empty());
    }

    #[test]
    fn a_name_longer_than_the_bound_is_refused() {
        // The cost of accepting it is not this node's: it is every node
        // on the network storing it until the service withdraws.
        assert_eq!(
            named("a".repeat(MAX_NAME_CHARS + 1)).validate(),
            Err(RegistryError::MalformedBranding)
        );
        assert_eq!(
            named("a".repeat(MAX_NAME_CHARS)).validate(),
            Ok(()),
            "the bound itself must be usable"
        );
    }

    #[test]
    fn a_description_longer_than_the_bound_is_refused() {
        let over = ServiceBranding {
            description: Some("a".repeat(MAX_DESCRIPTION_CHARS + 1)),
            ..ServiceBranding::default()
        };
        assert_eq!(over.validate(), Err(RegistryError::MalformedBranding));
    }

    #[test]
    fn the_bound_counts_characters_rather_than_bytes() {
        // A name of Chinese characters is three bytes each. Counting
        // bytes would let an English operator have 64 characters and a
        // Chinese one 21, which is a bound on who may register rather
        // than on how much they may store.
        assert_eq!(named("网".repeat(MAX_NAME_CHARS)).validate(), Ok(()));
    }

    #[test]
    fn an_empty_string_is_refused_because_it_is_a_none_in_disguise() {
        assert_eq!(
            named(String::new()).validate(),
            Err(RegistryError::MalformedBranding)
        );
    }

    #[test]
    fn a_name_that_would_render_as_something_else_is_refused() {
        for hostile in [
            "Two\nLines",
            "Trailing\u{0000}",
            // Renders as `gpj.exe` — the trick that has been getting
            // past file-name checks for a decade.
            "invoice\u{202E}gpj.exe",
            "\u{2066}spoofed\u{2069}",
        ] {
            assert_eq!(
                named(hostile).validate(),
                Err(RegistryError::MalformedBranding),
                "{hostile:?} must not become a service name"
            );
        }
    }

    #[test]
    fn names_that_are_not_latin_are_accepted() {
        // The rule refuses characters that misrender, not scripts.
        for name in ["Huduma za Kenya", "خدمات", "开放法币节点", "Nœud Français"] {
            assert_eq!(
                named(name).validate(),
                Ok(()),
                "{name} must be a usable name"
            );
        }
    }

    #[test]
    fn a_logo_that_is_a_url_or_a_path_is_refused() {
        // The whole point of the CID: a URL here would make every
        // viewer of the directory report themselves to whoever hosts it.
        for hostile in [
            "https://tracker.example/pixel.png",
            "/etc/passwd",
            "../../secret",
            "not-a-cid",
            "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG",
        ] {
            let with_logo = ServiceBranding {
                logo: Some(hostile.to_string()),
                ..ServiceBranding::default()
            };
            assert_eq!(
                with_logo.validate(),
                Err(RegistryError::MalformedBranding),
                "{hostile:?} must not become a logo"
            );
        }
    }

    #[test]
    fn a_website_that_is_not_http_is_refused() {
        // `javascript:` in an href is script execution in the viewer's
        // own page, from a string a stranger signed.
        for hostile in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///etc/passwd",
            "openfiat.allenhark.com",
        ] {
            assert_eq!(
                site(hostile).validate(),
                Err(RegistryError::MalformedBranding),
                "{hostile:?} must not become a website"
            );
        }
    }

    #[test]
    fn a_website_in_a_domain_that_can_never_resolve_is_refused() {
        // Same rule endpoints follow, and for the same reason: five
        // `.test` hostnames were once served to users as real
        // infrastructure.
        assert_eq!(
            site("https://openfiat.test").validate(),
            Err(RegistryError::MalformedBranding)
        );
    }

    #[test]
    fn http_is_allowed_because_a_homepage_is_not_an_api() {
        // `--public-rpc-url` must be HTTPS — a browser cannot call a
        // plain-HTTP node from an HTTPS page. A homepage is a link the
        // reader follows in a new tab, and refusing http here would
        // reject somebody's real site to no benefit.
        assert_eq!(site("http://openfiat.allenhark.com").validate(), Ok(()));
    }
}
