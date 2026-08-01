//! The payment methods this build ships, and the countries each one is
//! suggested in.
//!
//! # A suggestion list, not a validation gate
//!
//! Nothing consults this table to decide whether an advertisement is
//! valid. `openfiat_advertisements` checks the *form* of a
//! [`crate::PaymentMethodRef`] and deliberately not its membership here,
//! for the reason `openfiat_types::FiatCurrency` already gives: a node
//! built before a rail was added must not reject an advertisement that
//! names it, or two honest nodes on different releases disagree about
//! which advertisements exist.
//!
//! The same goes one level up, for the country lists. A merchant in Kenya
//! is *offered* M-Pesa; a merchant in Kenya who settles over Wise picks
//! Wise, which is listed for no country in particular, and a merchant who
//! settles over something this table has never heard of defines it
//! themselves ([`crate::MerchantPaymentMethod`]). `countries` orders a
//! picker. It does not shorten it.
//!
//! # Where the country lists come from, and what `None` means
//!
//! They are hand-written, like every other row here, and they are written
//! from what each operator actually serves — never derived from a name.
//! Deriving would be the same defect as taking a currency's flag from
//! whichever country mentioned the code first, which is how the South
//! African rand nearly ended up under a Zimbabwean flag: a plausible
//! guess, silently wrong, and impossible to notice from the code.
//!
//! `None` is therefore a deliberate answer and not a missing one. It says
//! *this build makes no per-country claim about this rail*, and it is used
//! for exactly two kinds of row:
//!
//! - the rails that genuinely exist everywhere — cash, and the two
//!   generic bank transfers named by function rather than by operator;
//! - the global fintechs (PayPal, Wise, Skrill, Revolut) whose country
//!   coverage is large, changes constantly, and differs by what the user
//!   is doing. A list of forty country codes that is wrong in three of
//!   them is worse than no list, because a picker would hide the rail in
//!   those three.
//!
//! A client shows `None` rows in every country, after the suggested ones.

use crate::record::{PaymentMethod, PaymentMethodCategory, PaymentMethodRef};
use std::sync::OnceLock;

use PaymentMethodCategory::{BankTransfer, Cash, Fintech, MobileMoney};

/// Every method this build suggests, in the order a picker should show
/// them absent a country to sort by.
pub fn catalog() -> &'static [PaymentMethod] {
    static BUILT: OnceLock<Vec<PaymentMethod>> = OnceLock::new();
    BUILT.get_or_init(|| {
        CATALOG
            .iter()
            .map(|(id, name, category, aliases, countries)| PaymentMethod {
                id: PaymentMethodRef::builtin(id)
                    .unwrap_or_else(|_| panic!("catalog id {id:?} is not a well-formed slug")),
                name: (*name).to_string(),
                category: *category,
                aliases: aliases.iter().map(|alias| (*alias).to_string()).collect(),
                countries: countries
                    .map(|codes| codes.iter().map(|code| (*code).to_string()).collect()),
            })
            .collect()
    })
}

/// The catalog split for one country: what to put at the top of the
/// picker, and what to put below it.
///
/// `suggested` is the rails that name this country; `others` is everything
/// else, still selectable, in catalog order. Nothing is withheld — a
/// merchant travelling, or serving a corridor this build has not thought
/// about, must still be able to reach every row.
///
/// An unrecognised country code is not an error. It yields an empty
/// `suggested` and the whole catalog in `others`, which is the truthful
/// answer: this build has nothing to suggest and everything to offer.
pub fn for_country(
    country: Option<&str>,
) -> (Vec<&'static PaymentMethod>, Vec<&'static PaymentMethod>) {
    catalog().iter().partition(|method| {
        country.is_some_and(|code| {
            method
                .countries
                .as_ref()
                .is_some_and(|listed| listed.iter().any(|listed| listed == code))
        })
    })
}

/// `(id, display name, category, aliases, countries)`.
///
/// The id is the stable half and the name is the readable one. They are
/// separate columns rather than one derived from the other because an
/// advertisement references the id: deriving it from the name would mean
/// that correcting a spelling silently orphaned every advertisement that
/// had chosen the rail.
///
/// Aliases are lowercase — they are matched against lowercased typed input
/// — and are never shown.
type Row = (
    &'static str,
    &'static str,
    PaymentMethodCategory,
    &'static [&'static str],
    Option<&'static [&'static str]>,
);

/// The countries in the SEPA schemes' geographical scope. Written out
/// rather than approximated as "the EU", which would drop Switzerland,
/// Norway and the UK — three places a merchant is very likely to be.
const SEPA: &[&str] = &[
    "AD", "AT", "BE", "BG", "CH", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FR", "GB", "GG", "GI",
    "GR", "HR", "HU", "IE", "IM", "IS", "IT", "JE", "LI", "LT", "LU", "LV", "MC", "MT", "NL", "NO",
    "PL", "PT", "RO", "SE", "SI", "SK", "SM", "VA",
];

const CATALOG: &[Row] = &[
    // Cash first: it is the only rail that exists in every country, and a
    // market with no local electronic system is still tradeable through
    // it. OFS-2100 §13 names Cash Deposit specifically.
    (
        "cash-deposit",
        "Cash Deposit",
        Cash,
        &[
            "cash deposit",
            "bank deposit",
            "cash at bank",
            "deposit cash",
        ],
        None,
    ),
    (
        "cash-in-person",
        "Cash in Person",
        Cash,
        &[
            "cash",
            "cash in person",
            "face to face",
            "f2f",
            "meet in person",
        ],
        None,
    ),
    // The two generic transfers. Named by what they are rather than by an
    // operator, so they are the fallback anywhere a local scheme is not
    // listed.
    (
        "bank-transfer",
        "Bank Transfer",
        BankTransfer,
        &["bank", "bank transfer", "local bank"],
        None,
    ),
    (
        "wire-transfer",
        "Wire Transfer",
        BankTransfer,
        &["swift", "wire", "swift wire"],
        None,
    ),
    // Mobile money.
    (
        "mpesa-kenya",
        "M-Pesa Kenya (Safaricom)",
        MobileMoney,
        &["mpesa", "m-pesa", "safaricom"],
        Some(&["KE"]),
    ),
    (
        "mpesa-pochi",
        "Mpesa Pochi la Biashara",
        MobileMoney,
        &["pochi", "pochi la biashara"],
        Some(&["KE"]),
    ),
    (
        "mpesa-mozambique",
        "M-Pesa Mozambique",
        MobileMoney,
        &["mpesa mozambique"],
        Some(&["MZ"]),
    ),
    (
        "mtn-momo",
        "MTN Mobile Money",
        MobileMoney,
        &["mtn", "momo", "mtn momo"],
        Some(&[
            "BJ", "CG", "CI", "CM", "GH", "GN", "GW", "LR", "NG", "RW", "SD", "SS", "SZ", "UG",
            "ZA", "ZM",
        ]),
    ),
    (
        "airtel-money",
        "Airtel Money",
        MobileMoney,
        &["airtel"],
        Some(&[
            "CD", "CG", "GA", "KE", "MG", "MW", "NE", "NG", "RW", "SC", "TD", "TZ", "UG", "ZM",
        ]),
    ),
    (
        "tigo-pesa",
        "Tigo Pesa",
        MobileMoney,
        &["tigo pesa", "tigo tanzania", "mixx by yas"],
        Some(&["TZ"]),
    ),
    (
        "orange-money",
        "Orange Money",
        MobileMoney,
        &["orange"],
        Some(&[
            "BF", "BW", "CD", "CF", "CI", "CM", "EG", "GN", "GW", "JO", "LR", "MA", "MG", "ML",
            "NE", "SL", "SN", "TN",
        ]),
    ),
    (
        "vodafone-cash",
        "Vodafone Cash",
        MobileMoney,
        &["vodafone"],
        Some(&["EG", "GH"]),
    ),
    (
        "telebirr",
        "Telebirr",
        MobileMoney,
        &["telebirr ethiopia"],
        Some(&["ET"]),
    ),
    (
        "ecocash",
        "EcoCash",
        MobileMoney,
        &["ecocash zimbabwe"],
        Some(&["ZW"]),
    ),
    (
        "gcash",
        "GCash",
        MobileMoney,
        &["gcash philippines"],
        Some(&["PH"]),
    ),
    ("maya", "Maya", MobileMoney, &["paymaya"], Some(&["PH"])),
    ("bkash", "bKash", MobileMoney, &["bkash"], Some(&["BD"])),
    ("nagad", "Nagad", MobileMoney, &[], Some(&["BD"])),
    ("easypaisa", "EasyPaisa", MobileMoney, &[], Some(&["PK"])),
    (
        "jazzcash",
        "JazzCash",
        MobileMoney,
        &["jazz"],
        Some(&["PK"]),
    ),
    (
        "esewa",
        "eSewa",
        MobileMoney,
        &["esewa nepal"],
        Some(&["NP"]),
    ),
    (
        "khalti",
        "Khalti",
        MobileMoney,
        &["khalti nepal"],
        Some(&["NP"]),
    ),
    (
        "wing",
        "Wing",
        MobileMoney,
        &["wing cambodia"],
        Some(&["KH"]),
    ),
    (
        "zain-cash",
        "Zain Cash",
        MobileMoney,
        &["zaincash"],
        Some(&["IQ", "JO", "SD"]),
    ),
    (
        "tigo-money",
        "Tigo Money",
        MobileMoney,
        &["tigo money"],
        Some(&["BO", "GT", "HN", "PY", "SV"]),
    ),
    // Bank rails, local schemes first.
    (
        "im-bank",
        "I&M Bank",
        BankTransfer,
        &["i&m", "im bank", "imb"],
        Some(&["KE", "RW", "TZ", "UG"]),
    ),
    (
        "equity-bank",
        "Equity Bank",
        BankTransfer,
        &["equity"],
        Some(&["CD", "KE", "RW", "SS", "TZ", "UG"]),
    ),
    (
        "kcb",
        "KCB",
        BankTransfer,
        &["kcb bank", "kenya commercial bank"],
        Some(&["BI", "KE", "RW", "SS", "TZ", "UG"]),
    ),
    (
        "sepa",
        "SEPA",
        BankTransfer,
        &["sepa transfer", "iban"],
        Some(SEPA),
    ),
    (
        "faster-payments-uk",
        "Faster Payments (UK)",
        BankTransfer,
        // Deliberately not the bare "fps": Hong Kong's Faster Payment
        // System answers to that, and a merchant handed the wrong
        // country's rail would be advertising one they cannot receive on.
        &["faster payments uk"],
        Some(&["GB"]),
    ),
    (
        "fps-hk",
        "FPS (Faster Payment System)",
        BankTransfer,
        &["fps", "fps hong kong", "轉數快"],
        Some(&["HK"]),
    ),
    ("ach", "ACH", BankTransfer, &["ach transfer"], Some(&["US"])),
    (
        "promptpay",
        "PromptPay",
        BankTransfer,
        &["promptpay thailand"],
        Some(&["TH"]),
    ),
    (
        "interac",
        "Interac e-Transfer",
        BankTransfer,
        &["interac", "e-transfer"],
        Some(&["CA"]),
    ),
    (
        "payid",
        "PayID",
        BankTransfer,
        &["payid australia", "osko"],
        Some(&["AU"]),
    ),
    (
        "spei",
        "SPEI",
        BankTransfer,
        &["spei mexico"],
        Some(&["MX"]),
    ),
    (
        "taiwan-pay",
        "Taiwan Pay",
        BankTransfer,
        &["twqr"],
        Some(&["TW"]),
    ),
    (
        "blik",
        "BLIK",
        BankTransfer,
        &["blik poland"],
        Some(&["PL"]),
    ),
    (
        "swish",
        "Swish",
        BankTransfer,
        &["swish sweden"],
        Some(&["SE"]),
    ),
    (
        "vipps",
        "Vipps",
        BankTransfer,
        &["vipps norway"],
        Some(&["NO"]),
    ),
    (
        "mobilepay",
        "MobilePay",
        BankTransfer,
        &["mobilepay denmark"],
        Some(&["DK", "FI"]),
    ),
    (
        "twint",
        "TWINT",
        BankTransfer,
        &["twint switzerland"],
        Some(&["CH"]),
    ),
    (
        "aba-pay",
        "ABA Pay",
        BankTransfer,
        &["aba", "aba bank"],
        Some(&["KH"]),
    ),
    (
        "cliq",
        "CliQ",
        BankTransfer,
        &["cliq jordan"],
        Some(&["JO"]),
    ),
    (
        "fawran",
        "Fawran",
        BankTransfer,
        &["fawran qatar"],
        Some(&["QA"]),
    ),
    (
        "knet",
        "KNET",
        BankTransfer,
        &["knet kuwait"],
        Some(&["KW"]),
    ),
    (
        "lankapay",
        "LankaPay",
        BankTransfer,
        &["ceft"],
        Some(&["LK"]),
    ),
    (
        "bcel-one",
        "BCEL One",
        BankTransfer,
        &["bcel"],
        Some(&["LA"]),
    ),
    (
        "sinpe-movil",
        "SINPE Movil",
        BankTransfer,
        &["sinpe"],
        Some(&["CR"]),
    ),
    (
        "juice-mcb",
        "Juice by MCB",
        BankTransfer,
        &["juice mauritius", "mcb juice"],
        Some(&["MU"]),
    ),
    // Fintech wallets.
    ("toss", "Toss", Fintech, &["toss korea"], Some(&["KR"])),
    ("kakaopay", "KakaoPay", Fintech, &["kakao"], Some(&["KR"])),
    (
        "kaspi",
        "Kaspi.kz",
        Fintech,
        &["kaspi", "kaspi gold"],
        Some(&["KZ"]),
    ),
    ("idram", "Idram", Fintech, &["idram armenia"], Some(&["AM"])),
    (
        "payme-uz",
        "Payme (Uzbekistan)",
        Fintech,
        &["payme uzbekistan"],
        Some(&["UZ"]),
    ),
    (
        "click-uz",
        "Click (Uzbekistan)",
        Fintech,
        &["click uzbekistan"],
        Some(&["UZ"]),
    ),
    ("qpay", "QPay", Fintech, &["qpay mongolia"], Some(&["MN"])),
    (
        "benefitpay",
        "BenefitPay",
        Fintech,
        &["benefit pay bahrain"],
        Some(&["BH"]),
    ),
    (
        "thawani",
        "Thawani",
        Fintech,
        &["thawani oman"],
        Some(&["OM"]),
    ),
    ("d17", "D17", Fintech, &["d17 tunisia"], Some(&["TN"])),
    (
        "baridimob",
        "BaridiMob",
        Fintech,
        &["baridimob algeria", "baridi"],
        Some(&["DZ"]),
    ),
    ("yappy", "Yappy", Fintech, &["yappy panama"], Some(&["PA"])),
    ("lynk", "Lynk", Fintech, &["lynk jamaica"], Some(&["JM"])),
    (
        "wipay",
        "WiPay",
        Fintech,
        &["wipay caribbean"],
        Some(&["BB", "JM", "TT"]),
    ),
    (
        "tpago",
        "tPago",
        Fintech,
        &["tpago dominican"],
        Some(&["DO"]),
    ),
    ("papara", "Papara", Fintech, &[], Some(&["TR"])),
    ("zelle", "Zelle", Fintech, &[], Some(&["US"])),
    (
        "upi",
        "UPI",
        Fintech,
        &["upi india", "bhim", "gpay", "phonepe"],
        Some(&["IN"]),
    ),
    ("pix", "PIX", Fintech, &["pix brazil"], Some(&["BR"])),
    (
        "alipay",
        "Alipay",
        Fintech,
        &["alipay china"],
        Some(&["CN"]),
    ),
    (
        "wechat-pay",
        "WeChat Pay",
        Fintech,
        &["wechat"],
        Some(&["CN"]),
    ),
    (
        "jkopay",
        "JKOPay",
        Fintech,
        &["jko", "街口支付"],
        Some(&["TW"]),
    ),
    (
        "line-pay",
        "LINE Pay",
        Fintech,
        &["line"],
        Some(&["JP", "TH", "TW"]),
    ),
    // The Hong Kong and Macau wallets are listed separately from their
    // mainland namesakes on purpose: an AlipayHK account cannot receive
    // from a mainland Alipay one, and a merchant offered the wrong half of
    // the brand is advertising a rail they cannot be paid on.
    (
        "payme-hk",
        "PayMe (HSBC Hong Kong)",
        Fintech,
        &["payme hsbc"],
        Some(&["HK"]),
    ),
    (
        "alipay-hk",
        "AlipayHK",
        Fintech,
        &["支付寶香港"],
        Some(&["HK"]),
    ),
    (
        "wechat-pay-hk",
        "WeChat Pay HK",
        Fintech,
        &["weixin hk"],
        Some(&["HK"]),
    ),
    (
        "octopus",
        "Octopus (O! ePay)",
        Fintech,
        &["octopus", "八達通"],
        Some(&["HK"]),
    ),
    (
        "mpay-macau",
        "MPay",
        Fintech,
        &["macau pass", "澳門通"],
        Some(&["MO"]),
    ),
    (
        "boc-pay",
        "BOC Pay",
        Fintech,
        &["bank of china pay"],
        Some(&["HK", "MO"]),
    ),
    (
        "mercado-pago",
        "Mercado Pago",
        Fintech,
        &["mercadopago"],
        Some(&["AR", "BR", "CL", "CO", "MX", "PE", "UY"]),
    ),
    // No country claim: see the module doc. Widely available, changing
    // constantly, and a wrong list would hide them where they do work.
    ("revolut", "Revolut", Fintech, &["rev"], None),
    ("wise", "Wise", Fintech, &["transferwise"], None),
    ("skrill", "Skrill", Fintech, &[], None),
    ("paypal", "PayPal", Fintech, &["pp"], None),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name::{check_name, skeleton};
    use std::collections::HashMap;

    /// Cash is the floor of what this network can do. Every other rail is
    /// a local system that may or may not exist where a user is; dropping
    /// cash would quietly make a country with no electronic rails
    /// untradeable.
    #[test]
    fn cash_is_offered_everywhere_because_it_is_the_only_universal_rail() {
        for id in ["cash-deposit", "cash-in-person"] {
            let method = catalog()
                .iter()
                .find(|m| m.id.as_str() == format!("builtin:{id}"))
                .unwrap_or_else(|| panic!("{id} must be listed"));
            assert_eq!(method.category, PaymentMethodCategory::Cash);
            assert!(
                method.countries.is_none(),
                "cash is not a claim about a country"
            );
        }
        let (_, others) = for_country(Some("XX"));
        assert!(
            others.iter().any(|m| m.name == "Cash in Person"),
            "an unrecognised country still gets the whole catalog"
        );
    }

    /// The three the feature was asked for, end to end: the country goes
    /// in, the rail a merchant there actually uses comes out.
    #[test]
    fn a_merchant_is_suggested_the_rail_their_country_runs_on() {
        for (country, expected) in [
            ("KE", "M-Pesa Kenya (Safaricom)"),
            ("BR", "PIX"),
            ("DE", "SEPA"),
            ("NG", "MTN Mobile Money"),
            ("PH", "GCash"),
        ] {
            let (suggested, _) = for_country(Some(country));
            assert!(
                suggested.iter().any(|m| m.name == expected),
                "a merchant in {country} must be offered {expected}, got {:?}",
                suggested.iter().map(|m| &m.name).collect::<Vec<_>>()
            );
        }
    }

    /// Suggestions are an ordering, never a restriction. A Kenyan
    /// merchant who settles over SEPA has to be able to say so.
    #[test]
    fn nothing_is_withheld_from_a_country_that_was_not_listed_for_it() {
        let (suggested, others) = for_country(Some("KE"));
        assert_eq!(
            suggested.len() + others.len(),
            catalog().len(),
            "every row must appear on exactly one side of the split"
        );
        assert!(others.iter().any(|m| m.name == "SEPA"));
        assert!(!suggested.iter().any(|m| m.name == "SEPA"));
    }

    /// With no country to sort by, nothing is suggested and everything is
    /// offered — rather than a partial answer that looks authoritative.
    #[test]
    fn no_country_means_no_suggestions_and_the_whole_catalog() {
        let (suggested, others) = for_country(None);
        assert!(suggested.is_empty());
        assert_eq!(others.len(), catalog().len());
    }

    /// An id is what an advertisement stores, so a duplicate would make
    /// one of the two rows unreachable and which one wins would depend on
    /// whether a client built its map forwards or backwards.
    #[test]
    fn every_id_is_unique_and_every_name_is_renderable() {
        let mut ids = HashMap::new();
        for method in catalog() {
            assert!(
                ids.insert(method.id.as_str(), &method.name).is_none(),
                "{} is listed twice",
                method.id.as_str()
            );
            assert_eq!(
                check_name(&method.name),
                Ok(()),
                "{:?} is a name this build would refuse from a merchant",
                method.name
            );
        }
    }

    /// The catalog is the thing merchant-defined names are checked
    /// against, so two catalog rows that fold to one skeleton would be
    /// two rows a person cannot tell apart — and the check would be
    /// rejecting a name that matches "one" of them without being able to
    /// say which.
    ///
    /// This is the test that found three real collisions: `Payme`
    /// (Uzbekistan) against `PayMe` (Hong Kong), the alias `tigo` claimed
    /// by both Tigo Pesa and Tigo Money, and the alias `wire` claimed by
    /// both Bank Transfer and Wire Transfer.
    #[test]
    fn no_two_rails_answer_to_the_same_skeleton() {
        let mut seen: HashMap<String, &str> = HashMap::new();
        for method in catalog() {
            let spellings = std::iter::once(&method.name).chain(method.aliases.iter());
            let mut mine: Vec<String> = spellings.map(|s| skeleton(s)).collect();
            mine.sort();
            mine.dedup();
            for folded in mine {
                if let Some(other) = seen.insert(folded.clone(), &method.name) {
                    assert_eq!(
                        other, method.name,
                        "{folded:?} is how both {other:?} and {:?} are spelled",
                        method.name
                    );
                }
            }
        }
    }

    /// An alias is matched against lowercased typed input, so one
    /// carrying an uppercase letter could never be typed into existence —
    /// it would sit in the table looking like it worked.
    #[test]
    fn aliases_are_lowercase_because_that_is_what_they_are_matched_against() {
        for method in catalog() {
            for alias in &method.aliases {
                assert_eq!(
                    *alias,
                    alias.to_lowercase(),
                    "{alias:?} on {} could never match typed input",
                    method.name
                );
            }
        }
    }

    /// A country list is read by a client as a key into its own country
    /// table, so a malformed code is a row that matches nothing. An empty
    /// list is worse than malformed: it means "suggested nowhere", which
    /// no row should ever say — `None` is how a row declines to make a
    /// claim.
    #[test]
    fn every_country_code_is_one_a_client_can_key_off() {
        for method in catalog() {
            let Some(countries) = &method.countries else {
                continue;
            };
            assert!(!countries.is_empty(), "{} lists nowhere", method.name);
            let mut sorted = countries.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(
                &sorted, countries,
                "{}'s countries must be sorted and free of duplicates",
                method.name
            );
            for code in countries {
                assert!(
                    (2..=3).contains(&code.len()) && code.chars().all(|c| c.is_ascii_uppercase()),
                    "{code:?} on {} is not a country code",
                    method.name
                );
            }
        }
    }
}
