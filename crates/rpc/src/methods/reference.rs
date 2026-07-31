//! Reference data: the countries, fiat currencies, payment methods and
//! token mints an interface offers a user to choose from.
//!
//! # This is a hardcoded table, and that is the point
//!
//! Nothing below is derived. The countries, currencies and payment
//! methods are lists compiled into this node, and calling them
//! "authoritative" would be flattery. What they are instead is *one*
//! list, in one place, that every client reads — which is the whole of
//! the improvement and is worth stating plainly rather than dressing up.
//! The mints are not this module's own table either: they come verbatim
//! from `openfiat_chain::mints::KNOWN_MINTS`, itself compiled in and
//! transcribed from the escrow program's shipped settlement mints.
//! Nothing here is read off the chain at runtime.
//!
//! Before this method existed each interface shipped its own copy. The
//! web app's was 441 lines of countries and 84 payment methods; another
//! client's would have been a different 441 lines. Two honest builds
//! could therefore disagree about what the network supports, and adding a
//! payment method meant shipping a new release of every interface that
//! wanted to offer it. Moving the table here does not make it any less
//! hand-written; it makes it one artefact that a node operator updates
//! and every client picks up on the next call.
//!
//! The mints are the sharpest version of the same problem, because there
//! a private copy does not merely disagree — it silently matches nothing.
//! The web app gated its `/[asset]/[currency]` routes on a hand-written
//! `["USDT", "USDC", "USD1", "SOL"]`, so `/sol/kes` could never show an
//! advertisement (this network settles wrapped SOL, and the mint is named
//! `wSOL`), `/usd1/kes` drew a coin mark for a token no mint here is
//! named after, and `tUSDC` — the Token-2022 mint the running devnet
//! denominates its fee treasuries in — had no page at all. A permanently
//! empty market page is indistinguishable from an empty market.
//!
//! # This is a suggestion list, not a validation gate
//!
//! Nothing on this surface consults it. An advertisement in a currency
//! absent from `CURRENCIES` is accepted exactly as before, because
//! `openfiat_types::FiatCurrency` checks the *form* of a code and
//! deliberately not its membership of any list — see that type's own
//! comment for why a membership check would have two honest nodes on
//! different releases disagreeing about which advertisements are valid.
//! The same reasoning applies one level up: a merchant who trades a rail
//! this build has never heard of must still be able to name it, and the
//! web app's picker still lets them type one in.
//!
//! So this answers "what should I put in the dropdown", never "is this
//! allowed".
//!
//! The mints carry that distinction on their own terms and it is sharper
//! there, because a real enforcement list exists elsewhere: the
//! settlement allowlist lives on chain in the escrow program's
//! `FeeConfig` and governance can change it. `mints` is a phrasebook for
//! turning an address into a name, and the two sets are not guaranteed to
//! be equal in either direction — see [`ReferenceData::mints`].
//!
//! # What is deliberately not here
//!
//! Flag emoji, URL slugs and "which currencies are popular" are
//! presentation decisions belonging to one interface, not facts about the
//! network — a flag is derivable from an ISO code anyway, and a slug is
//! part of a particular app's URL scheme. Recognition status is left out
//! for a stronger reason: a protocol-level table that sorts states into
//! recognised and not is an invitation to filter on it, and this network
//! has no business deciding whose money is tradeable.
//!
//! # Room for OFS-2100 per-country methods and merchant-created ones
//!
//! Payment methods come back as objects rather than bare strings
//! specifically so that per-country availability and a
//! merchant-registered origin can be added as fields without breaking a
//! client that already reads `name`, `category` and `aliases`. That work
//! is not started here and this module makes no attempt to guess at it —
//! inventing a `countries` list for eighty-four rails from their names
//! would be fabricating data, and a wrong availability list is worse than
//! none.

use crate::dispatch::{MethodTable, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_crypto::sha256;
use openfiat_storage::KvStore;
use openfiat_types::FiatCurrency;
use std::sync::OnceLock;

use PaymentMethodCategory::{BankTransfer, Cash, Fintech, MobileMoney};

/// A fiat currency an interface can offer, with the name and symbol to
/// print beside its code.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Currency {
    pub code: FiatCurrency,
    pub name: String,
    /// The symbol as written locally ("KSh", "₦", "£"). Not unique —
    /// eleven currencies here are written "$" — so it is decoration
    /// beside a code, never an identifier.
    pub symbol: String,
}

/// A country or territory, and the currencies it actually trades in.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Country {
    /// ISO 3166-1 alpha-2 where one exists. Northern Cyprus and
    /// Transnistria have no ISO code and no prospect of getting one, so
    /// they carry the stable pseudo-codes `XNC` and `XTR`; a client
    /// keying off this must not assume two characters.
    pub code: String,
    pub name: String,
    /// The currency most trade here is denominated in.
    pub currency: FiatCurrency,
    /// Other currencies in genuine everyday circulation, most-used first.
    ///
    /// A single currency per country is wrong where it matters most: in a
    /// dollarised economy the USD book is frequently the larger of the
    /// two, and a client that only ever offered `currency` would hide it.
    /// Empty for the great majority of countries.
    pub alt_currencies: Vec<FiatCurrency>,
}

/// Which kind of rail a payment method is, so an interface can group a
/// long list into something a person can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PaymentMethodCategory {
    MobileMoney,
    BankTransfer,
    Fintech,
    /// OFS-2100 §13 names Cash Deposit alongside the electronic rails.
    /// Cash is the only method that exists in every country, which is the
    /// point of listing it: a market with no local electronic system is
    /// still tradeable.
    Cash,
}

/// A payment method a merchant can advertise accepting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaymentMethod {
    /// The name as it should be shown and as it is stored on an
    /// advertisement. Comparisons elsewhere are on this exact string.
    pub name: String,
    pub category: PaymentMethodCategory,
    /// Spellings a person might type when they mean this method, for
    /// type-ahead. Lowercase. Never shown.
    pub aliases: Vec<String>,
}

/// A token mint this build knows a name for.
///
/// The address is the identity and the symbol is a nickname — see
/// [`ReferenceData::mints`] for why a client must never key off the
/// symbol, and for the difference between "this node can name it" and
/// "this network will settle in it".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Mint {
    /// Base58 mint address. The only field that identifies anything.
    pub mint: String,
    /// What people call it: `wSOL`, `USDC`, `tUSDC`. Cluster-dependent
    /// and not unique across clusters — `USDC` names one address on
    /// mainnet and a different one on devnet.
    pub symbol: String,
    /// Base-unit exponent, carried in the same row as the symbol so a
    /// client cannot know what to call a mint while guessing how to
    /// scale it. wSOL is 9 and the stablecoins here are 6; assuming 6
    /// for all of them renders SOL amounts a thousand times too large.
    pub decimals: u8,
}

/// Everything an interface needs to populate its country, currency,
/// payment-method and asset controls, in one answer.
///
/// One method rather than four because they are cross-referenced: a
/// [`Country`] names currency codes that must resolve in `currencies`. A
/// client assembling them from separate calls — or worse, from two nodes
/// — could hold a country pointing at a currency it never received.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReferenceData {
    /// A digest of the four lists below, first 16 hex characters of
    /// their SHA-256.
    ///
    /// It changes when and only when the data does, which is what makes
    /// it useful for two things a version number cannot do: a client can
    /// cache on it across releases that did not touch the table, and two
    /// nodes can be compared for agreement by one short string instead of
    /// a field-by-field diff of five hundred rows.
    pub revision: String,
    pub currencies: Vec<Currency>,
    pub countries: Vec<Country>,
    pub payment_methods: Vec<PaymentMethod>,
    /// The mints this build can put a name to, straight from
    /// `openfiat_chain::mints::KNOWN_MINTS` — itself a compiled-in table
    /// transcribed from the escrow program's shipped
    /// `DEFAULT_SETTLEMENT_MINTS`. Nothing here is read off the chain at
    /// runtime.
    ///
    /// # Naming a mint and allowing one are different questions
    ///
    /// The settlement allowlist lives on chain in the escrow program's
    /// `FeeConfig` and is governance-updatable. It is the enforcement;
    /// this list is a phrasebook. The two sets are related by having been
    /// written together and are not guaranteed to match: governance can
    /// allowlist a mint on Tuesday that no node built before Tuesday has
    /// a name for, and a mint named here can be removed from the
    /// allowlist without this build hearing about it. An interface that
    /// reads this as "what can be traded" will be wrong in both
    /// directions, and the wrong direction that costs someone money is
    /// offering a mint escrow will refuse.
    ///
    /// OPEN is deliberately absent even though it is the protocol's own
    /// token: OFS-4100 holds it off the escrow allowlist until the public
    /// sale, so naming it would put a familiar label on something no
    /// buyer can receive from escrow.
    ///
    /// # Look it up by address, never by symbol
    ///
    /// A symbol is spoofable and cluster-dependent, which is why no
    /// record on this protocol carries one — an advertisement carries a
    /// `MintAddress` and the name is resolved here, at the edge. A client
    /// that routes on `"SOL"` finds nothing, because the mint this
    /// network settles wrapped SOL through is named `wSOL`; that exact
    /// mismatch is what a hand-written ticker list in one interface
    /// produced.
    pub mints: Vec<Mint>,
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getReferenceData",
        method_fn(
            |_state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<&'static ReferenceData, RpcError> { Ok(reference_data()) },
        ),
    );
}

/// The built, parsed table. Built once: parsing five hundred rows on
/// every call would be pure waste for an answer that cannot change while
/// the process runs.
fn reference_data() -> &'static ReferenceData {
    static DATA: OnceLock<ReferenceData> = OnceLock::new();
    DATA.get_or_init(build)
}

/// # Panics
///
/// If any code in the tables below is not a well-formed currency code.
/// That is deliberate: a malformed code would otherwise be silently
/// dropped from the answer, and an interface would show a country with no
/// currency and no explanation. `every_code_in_the_tables_is_one_the_protocol_can_carry`
/// turns the panic into a test failure before it can reach a node.
fn build() -> ReferenceData {
    let currency = |code: &str| {
        FiatCurrency::parse(code)
            .unwrap_or_else(|_| panic!("reference table currency code {code:?} is malformed"))
    };

    let currencies: Vec<Currency> = CURRENCIES
        .iter()
        .map(|(code, name, symbol)| Currency {
            code: currency(code),
            name: (*name).to_string(),
            symbol: (*symbol).to_string(),
        })
        .collect();

    let countries: Vec<Country> = COUNTRIES
        .iter()
        .map(|(code, name, primary, alts)| Country {
            code: (*code).to_string(),
            name: (*name).to_string(),
            currency: currency(primary),
            alt_currencies: alts.iter().map(|alt| currency(alt)).collect(),
        })
        .collect();

    let payment_methods: Vec<PaymentMethod> = PAYMENT_METHODS
        .iter()
        .map(|(name, category, aliases)| PaymentMethod {
            name: (*name).to_string(),
            category: *category,
            aliases: aliases.iter().map(|a| (*a).to_string()).collect(),
        })
        .collect();

    // Not a table of this module's own: the mints come from the one
    // place in this workspace that already answers "what is this address
    // called", so a client asking here and a node rendering an
    // advertisement cannot end up disagreeing.
    let mints: Vec<Mint> = openfiat_chain::mints::KNOWN_MINTS
        .iter()
        .map(|known| Mint {
            mint: known.mint.to_string(),
            symbol: known.symbol.to_string(),
            decimals: known.decimals,
        })
        .collect();

    // Over the four lists as they will be sent, so the digest tracks
    // exactly what a client received and nothing else.
    let bytes = serde_json::to_vec(&(&currencies, &countries, &payment_methods, &mints))
        .expect("reference tables are plain data and always serialize");
    let revision = sha256(&bytes)
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();

    ReferenceData {
        revision,
        currencies,
        countries,
        payment_methods,
        mints,
    }
}

/// Every fiat currency any country below trades in, by code.
/// Split out rather than repeated per country because forty-two
/// countries use the euro and none of them should get to disagree about
/// what it is called.
const CURRENCIES: &[(&str, &str, &str)] = &[
    ("AED", "UAE dirham", "د.إ"),
    ("AFN", "Afghan afghani", "؋"),
    ("ALL", "Albanian lek", "L"),
    ("AMD", "Armenian dram", "֏"),
    ("ANG", "Netherlands Antillean guilder", "ƒ"),
    ("AOA", "Angolan kwanza", "Kz"),
    ("ARS", "Argentine peso", "$"),
    ("AUD", "Australian dollar", "$"),
    ("AWG", "Aruban florin", "ƒ"),
    ("AZN", "Azerbaijani manat", "₼"),
    ("BAM", "Bosnia and Herzegovina convertible mark", "KM"),
    ("BBD", "Barbadian dollar", "Bds$"),
    ("BDT", "Bangladeshi taka", "৳"),
    ("BGN", "Bulgarian lev", "лв"),
    ("BHD", "Bahraini dinar", ".د.ب"),
    ("BIF", "Burundian franc", "FBu"),
    ("BMD", "Bermudian dollar", "$"),
    ("BND", "Brunei dollar", "B$"),
    ("BOB", "Bolivian boliviano", "Bs"),
    ("BRL", "Brazilian real", "R$"),
    ("BSD", "Bahamian dollar", "B$"),
    ("BTN", "Bhutanese ngultrum", "Nu."),
    ("BWP", "Botswana pula", "P"),
    ("BYN", "Belarusian ruble", "Br"),
    ("BZD", "Belize dollar", "BZ$"),
    ("CAD", "Canadian dollar", "$"),
    ("CDF", "Congolese franc", "FC"),
    ("CHF", "Swiss franc", "CHF"),
    ("CLP", "Chilean peso", "$"),
    ("CNY", "Chinese yuan (renminbi)", "¥"),
    ("COP", "Colombian peso", "$"),
    ("CRC", "Costa Rican colón", "₡"),
    ("CUP", "Cuban peso", "₱"),
    ("CVE", "Cape Verdean escudo", "$"),
    ("CZK", "Czech koruna", "Kč"),
    ("DJF", "Djiboutian franc", "Fdj"),
    ("DKK", "Danish krone", "kr"),
    ("DOP", "Dominican peso", "RD$"),
    ("DZD", "Algerian dinar", "د.ج"),
    ("EGP", "Egyptian pound", "E£"),
    ("ERN", "Eritrean nakfa", "Nfk"),
    ("ETB", "Ethiopian birr", "Br"),
    ("EUR", "Euro", "€"),
    ("FJD", "Fijian dollar", "FJ$"),
    ("FKP", "Falkland Islands pound", "£"),
    ("GBP", "Pound sterling", "£"),
    ("GEL", "Georgian lari", "₾"),
    ("GGP", "Guernsey pound", "£"),
    ("GHS", "Ghanaian cedi", "GH₵"),
    ("GIP", "Gibraltar pound", "£"),
    ("GMD", "Gambian dalasi", "D"),
    ("GNF", "Guinean franc", "FG"),
    ("GTQ", "Guatemalan quetzal", "Q"),
    ("GYD", "Guyanese dollar", "G$"),
    ("HKD", "Hong Kong dollar", "HK$"),
    ("HNL", "Honduran lempira", "L"),
    ("HTG", "Haitian gourde", "G"),
    ("HUF", "Hungarian forint", "Ft"),
    ("IDR", "Indonesian rupiah", "Rp"),
    ("ILS", "Israeli new shekel", "₪"),
    ("IMP", "Manx pound", "£"),
    ("INR", "Indian rupee", "₹"),
    ("IQD", "Iraqi dinar", "ع.د"),
    ("IRR", "Iranian rial", "﷼"),
    ("ISK", "Icelandic króna", "kr"),
    ("JEP", "Jersey pound", "£"),
    ("JMD", "Jamaican dollar", "J$"),
    ("JOD", "Jordanian dinar", "د.ا"),
    ("JPY", "Japanese yen", "¥"),
    ("KES", "Kenyan shilling", "KSh"),
    ("KGS", "Kyrgyz som", "с"),
    ("KHR", "Cambodian riel", "៛"),
    ("KMF", "Comorian franc", "CF"),
    ("KPW", "North Korean won", "₩"),
    ("KRW", "South Korean won", "₩"),
    ("KWD", "Kuwaiti dinar", "د.ك"),
    ("KYD", "Cayman Islands dollar", "CI$"),
    ("KZT", "Kazakhstani tenge", "₸"),
    ("LAK", "Lao kip", "₭"),
    ("LBP", "Lebanese pound", "ل.ل"),
    ("LKR", "Sri Lankan rupee", "Rs"),
    ("LRD", "Liberian dollar", "L$"),
    ("LSL", "Lesotho loti", "L"),
    ("LYD", "Libyan dinar", "ل.د"),
    ("MAD", "Moroccan dirham", "د.م."),
    ("MDL", "Moldovan leu", "L"),
    ("MGA", "Malagasy ariary", "Ar"),
    ("MKD", "Macedonian denar", "ден"),
    ("MMK", "Myanmar kyat", "K"),
    ("MNT", "Mongolian tögrög", "₮"),
    ("MOP", "Macanese pataca", "MOP$"),
    ("MRU", "Mauritanian ouguiya", "UM"),
    ("MUR", "Mauritian rupee", "₨"),
    ("MVR", "Maldivian rufiyaa", "Rf"),
    ("MWK", "Malawian kwacha", "MK"),
    ("MXN", "Mexican peso", "$"),
    ("MYR", "Malaysian ringgit", "RM"),
    ("MZN", "Mozambican metical", "MT"),
    ("NAD", "Namibian dollar", "N$"),
    ("NGN", "Nigerian naira", "₦"),
    ("NIO", "Nicaraguan córdoba", "C$"),
    ("NOK", "Norwegian krone", "kr"),
    ("NPR", "Nepalese rupee", "₨"),
    ("NZD", "New Zealand dollar", "$"),
    ("OMR", "Omani rial", "ر.ع."),
    ("PAB", "Panamanian balboa (USD also used)", "B/."),
    ("PEN", "Peruvian sol", "S/"),
    ("PGK", "Papua New Guinean kina", "K"),
    ("PHP", "Philippine peso", "₱"),
    ("PKR", "Pakistani rupee", "₨"),
    ("PLN", "Polish złoty", "zł"),
    ("PRB", "Transnistrian ruble", "р."),
    ("PYG", "Paraguayan guaraní", "₲"),
    ("QAR", "Qatari riyal", "ر.ق"),
    ("RON", "Romanian leu", "lei"),
    ("RSD", "Serbian dinar", "din"),
    ("RUB", "Russian ruble", "₽"),
    ("RWF", "Rwandan franc", "FRw"),
    ("SAR", "Saudi riyal", "ر.س"),
    ("SBD", "Solomon Islands dollar", "SI$"),
    ("SCR", "Seychellois rupee", "₨"),
    ("SDG", "Sudanese pound", "ج.س."),
    ("SEK", "Swedish krona", "kr"),
    ("SGD", "Singapore dollar", "S$"),
    ("SHP", "Saint Helena pound", "£"),
    ("SLE", "Sierra Leonean leone", "Le"),
    ("SLS", "Somaliland shilling", "SlSh"),
    ("SOS", "Somali shilling", "Sh"),
    ("SRD", "Surinamese dollar", "$"),
    ("SSP", "South Sudanese pound", "£"),
    ("STN", "São Tomé and Príncipe dobra", "Db"),
    ("SYP", "Syrian pound", "£S"),
    ("SZL", "Swazi lilangeni", "E"),
    ("THB", "Thai baht", "฿"),
    ("TJS", "Tajikistani somoni", "ЅМ"),
    ("TMT", "Turkmenistani manat", "m"),
    ("TND", "Tunisian dinar", "د.ت"),
    ("TOP", "Tongan paʻanga", "T$"),
    ("TRY", "Turkish lira", "₺"),
    ("TTD", "Trinidad and Tobago dollar", "TT$"),
    ("TWD", "New Taiwan dollar", "NT$"),
    ("TZS", "Tanzanian shilling", "TSh"),
    ("UAH", "Ukrainian hryvnia", "₴"),
    ("UGX", "Ugandan shilling", "USh"),
    ("USD", "United States dollar", "$"),
    ("UYU", "Uruguayan peso", "$U"),
    ("UZS", "Uzbekistani so'm", "soʻm"),
    ("VES", "Venezuelan bolívar soberano", "Bs.S"),
    ("VND", "Vietnamese đồng", "₫"),
    ("VUV", "Vanuatu vatu", "VT"),
    ("WST", "Samoan tālā", "T"),
    ("XAF", "Central African CFA franc", "FCFA"),
    ("XCD", "East Caribbean dollar", "EC$"),
    ("XOF", "West African CFA franc", "CFA"),
    ("XPF", "CFP franc", "₣"),
    ("YER", "Yemeni rial", "﷼"),
    ("ZAR", "South African rand", "R"),
    ("ZMW", "Zambian kwacha", "K"),
    ("ZWG", "Zimbabwe Gold (ZiG)", "ZiG"),
];

/// Every country and territory with a currency in actual use, in the
/// order an interface should present them absent a better idea.
const COUNTRIES: &[(&str, &str, &str, &[&str])] = &[
    ("DZ", "Algeria", "DZD", &[]),
    ("AO", "Angola", "AOA", &[]),
    ("BJ", "Benin", "XOF", &[]),
    ("BW", "Botswana", "BWP", &[]),
    ("BF", "Burkina Faso", "XOF", &[]),
    ("BI", "Burundi", "BIF", &[]),
    ("CV", "Cabo Verde", "CVE", &[]),
    ("CM", "Cameroon", "XAF", &[]),
    ("CF", "Central African Republic", "XAF", &[]),
    ("TD", "Chad", "XAF", &[]),
    ("KM", "Comoros", "KMF", &[]),
    ("CD", "Congo (DRC)", "CDF", &[]),
    ("CG", "Congo (Republic)", "XAF", &[]),
    ("CI", "Côte d'Ivoire", "XOF", &[]),
    ("DJ", "Djibouti", "DJF", &[]),
    ("EG", "Egypt", "EGP", &[]),
    ("GQ", "Equatorial Guinea", "XAF", &[]),
    ("ER", "Eritrea", "ERN", &[]),
    ("SZ", "Eswatini", "SZL", &[]),
    ("ET", "Ethiopia", "ETB", &[]),
    ("GA", "Gabon", "XAF", &[]),
    ("GM", "Gambia", "GMD", &[]),
    ("GH", "Ghana", "GHS", &[]),
    ("GN", "Guinea", "GNF", &[]),
    ("GW", "Guinea-Bissau", "XOF", &[]),
    ("KE", "Kenya", "KES", &[]),
    ("LS", "Lesotho", "LSL", &[]),
    ("LR", "Liberia", "LRD", &[]),
    ("LY", "Libya", "LYD", &[]),
    ("MG", "Madagascar", "MGA", &[]),
    ("MW", "Malawi", "MWK", &[]),
    ("ML", "Mali", "XOF", &[]),
    ("MR", "Mauritania", "MRU", &[]),
    ("MU", "Mauritius", "MUR", &[]),
    ("MA", "Morocco", "MAD", &[]),
    ("MZ", "Mozambique", "MZN", &[]),
    ("NA", "Namibia", "NAD", &[]),
    ("NE", "Niger", "XOF", &[]),
    ("NG", "Nigeria", "NGN", &[]),
    ("RW", "Rwanda", "RWF", &[]),
    ("ST", "São Tomé & Príncipe", "STN", &[]),
    ("SN", "Senegal", "XOF", &[]),
    ("SC", "Seychelles", "SCR", &[]),
    ("SL", "Sierra Leone", "SLE", &[]),
    ("SO", "Somalia", "SOS", &["USD"]),
    ("ZA", "South Africa", "ZAR", &[]),
    ("SS", "South Sudan", "SSP", &["USD"]),
    ("SD", "Sudan", "SDG", &[]),
    ("TZ", "Tanzania", "TZS", &[]),
    ("TG", "Togo", "XOF", &[]),
    ("TN", "Tunisia", "TND", &[]),
    ("UG", "Uganda", "UGX", &[]),
    ("ZM", "Zambia", "ZMW", &["USD"]),
    ("ZW", "Zimbabwe", "ZWG", &["USD", "ZAR"]),
    ("EH", "Western Sahara", "MAD", &[]),
    ("XS", "Somaliland", "SLS", &[]),
    ("SH", "Saint Helena", "SHP", &[]),
    ("RE", "Réunion", "EUR", &[]),
    ("YT", "Mayotte", "EUR", &[]),
    ("AF", "Afghanistan", "AFN", &["USD"]),
    ("AM", "Armenia", "AMD", &[]),
    ("AZ", "Azerbaijan", "AZN", &[]),
    ("BH", "Bahrain", "BHD", &[]),
    ("BD", "Bangladesh", "BDT", &[]),
    ("BT", "Bhutan", "BTN", &[]),
    ("BN", "Brunei", "BND", &[]),
    ("KH", "Cambodia", "KHR", &["USD"]),
    ("CN", "China", "CNY", &[]),
    ("CY", "Cyprus", "EUR", &[]),
    ("GE", "Georgia", "GEL", &[]),
    ("IN", "India", "INR", &[]),
    ("ID", "Indonesia", "IDR", &[]),
    ("IR", "Iran", "IRR", &[]),
    ("IQ", "Iraq", "IQD", &["USD"]),
    ("IL", "Israel", "ILS", &[]),
    ("JP", "Japan", "JPY", &[]),
    ("JO", "Jordan", "JOD", &[]),
    ("KZ", "Kazakhstan", "KZT", &[]),
    ("KW", "Kuwait", "KWD", &[]),
    ("KG", "Kyrgyzstan", "KGS", &[]),
    ("LA", "Laos", "LAK", &["USD", "THB"]),
    ("LB", "Lebanon", "LBP", &["USD"]),
    ("MY", "Malaysia", "MYR", &[]),
    ("MV", "Maldives", "MVR", &[]),
    ("MN", "Mongolia", "MNT", &[]),
    ("MM", "Myanmar", "MMK", &["USD"]),
    ("NP", "Nepal", "NPR", &[]),
    ("KP", "North Korea", "KPW", &[]),
    ("OM", "Oman", "OMR", &[]),
    ("PK", "Pakistan", "PKR", &[]),
    ("PH", "Philippines", "PHP", &[]),
    ("QA", "Qatar", "QAR", &[]),
    ("SA", "Saudi Arabia", "SAR", &[]),
    ("SG", "Singapore", "SGD", &[]),
    ("KR", "South Korea", "KRW", &[]),
    ("LK", "Sri Lanka", "LKR", &[]),
    ("SY", "Syria", "SYP", &[]),
    ("TJ", "Tajikistan", "TJS", &[]),
    ("TH", "Thailand", "THB", &[]),
    ("TL", "Timor-Leste", "USD", &[]),
    ("TR", "Turkey", "TRY", &[]),
    ("TM", "Turkmenistan", "TMT", &[]),
    ("AE", "United Arab Emirates", "AED", &[]),
    ("UZ", "Uzbekistan", "UZS", &[]),
    ("VN", "Vietnam", "VND", &[]),
    ("YE", "Yemen", "YER", &[]),
    ("PS", "Palestine", "ILS", &["JOD", "USD"]),
    ("TW", "Taiwan", "TWD", &[]),
    ("HK", "Hong Kong", "HKD", &[]),
    ("MO", "Macau", "MOP", &[]),
    ("XNC", "Northern Cyprus", "TRY", &[]),
    ("IO", "British Indian Ocean Territory", "USD", &[]),
    ("CC", "Cocos (Keeling) Islands", "AUD", &[]),
    ("CX", "Christmas Island", "AUD", &[]),
    ("AL", "Albania", "ALL", &[]),
    ("AD", "Andorra", "EUR", &[]),
    ("AT", "Austria", "EUR", &[]),
    ("BY", "Belarus", "BYN", &[]),
    ("BE", "Belgium", "EUR", &[]),
    ("BA", "Bosnia & Herzegovina", "BAM", &[]),
    ("BG", "Bulgaria", "BGN", &[]),
    ("HR", "Croatia", "EUR", &[]),
    ("CZ", "Czechia", "CZK", &[]),
    ("DK", "Denmark", "DKK", &[]),
    ("EE", "Estonia", "EUR", &[]),
    ("FI", "Finland", "EUR", &[]),
    ("FR", "France", "EUR", &[]),
    ("DE", "Germany", "EUR", &[]),
    ("GR", "Greece", "EUR", &[]),
    ("HU", "Hungary", "HUF", &[]),
    ("IS", "Iceland", "ISK", &[]),
    ("IE", "Ireland", "EUR", &[]),
    ("IT", "Italy", "EUR", &[]),
    ("LV", "Latvia", "EUR", &[]),
    ("LI", "Liechtenstein", "CHF", &[]),
    ("LT", "Lithuania", "EUR", &[]),
    ("LU", "Luxembourg", "EUR", &[]),
    ("MT", "Malta", "EUR", &[]),
    ("MD", "Moldova", "MDL", &[]),
    ("MC", "Monaco", "EUR", &[]),
    ("ME", "Montenegro", "EUR", &[]),
    ("NL", "Netherlands", "EUR", &[]),
    ("MK", "North Macedonia", "MKD", &[]),
    ("NO", "Norway", "NOK", &[]),
    ("PL", "Poland", "PLN", &[]),
    ("PT", "Portugal", "EUR", &[]),
    ("RO", "Romania", "RON", &[]),
    ("RU", "Russia", "RUB", &[]),
    ("SM", "San Marino", "EUR", &[]),
    ("RS", "Serbia", "RSD", &[]),
    ("SK", "Slovakia", "EUR", &[]),
    ("SI", "Slovenia", "EUR", &[]),
    ("ES", "Spain", "EUR", &[]),
    ("SE", "Sweden", "SEK", &[]),
    ("CH", "Switzerland", "CHF", &[]),
    ("UA", "Ukraine", "UAH", &[]),
    ("GB", "United Kingdom", "GBP", &[]),
    ("VA", "Vatican City", "EUR", &[]),
    ("XK", "Kosovo", "EUR", &[]),
    ("XTR", "Transnistria", "PRB", &[]),
    ("AX", "Åland Islands", "EUR", &[]),
    ("FO", "Faroe Islands", "DKK", &[]),
    ("GI", "Gibraltar", "GIP", &[]),
    ("GG", "Guernsey", "GGP", &[]),
    ("JE", "Jersey", "JEP", &[]),
    ("IM", "Isle of Man", "IMP", &[]),
    ("SJ", "Svalbard & Jan Mayen", "NOK", &[]),
    ("US", "United States", "USD", &[]),
    ("CA", "Canada", "CAD", &[]),
    ("MX", "Mexico", "MXN", &[]),
    ("BZ", "Belize", "BZD", &[]),
    ("CR", "Costa Rica", "CRC", &[]),
    ("SV", "El Salvador", "USD", &[]),
    ("GT", "Guatemala", "GTQ", &[]),
    ("HN", "Honduras", "HNL", &[]),
    ("NI", "Nicaragua", "NIO", &[]),
    ("PA", "Panama", "PAB", &["USD"]),
    ("CU", "Cuba", "CUP", &["USD", "EUR"]),
    ("DO", "Dominican Republic", "DOP", &[]),
    ("HT", "Haiti", "HTG", &[]),
    ("JM", "Jamaica", "JMD", &[]),
    ("TT", "Trinidad & Tobago", "TTD", &[]),
    ("BB", "Barbados", "BBD", &[]),
    ("BS", "Bahamas", "BSD", &[]),
    ("AG", "Antigua & Barbuda", "XCD", &[]),
    ("DM", "Dominica", "XCD", &[]),
    ("GD", "Grenada", "XCD", &[]),
    ("KN", "Saint Kitts & Nevis", "XCD", &[]),
    ("LC", "Saint Lucia", "XCD", &[]),
    ("VC", "Saint Vincent & the Grenadines", "XCD", &[]),
    ("AI", "Anguilla", "XCD", &[]),
    ("AW", "Aruba", "AWG", &[]),
    ("BM", "Bermuda", "BMD", &[]),
    ("BQ", "Caribbean Netherlands", "USD", &[]),
    ("VG", "British Virgin Islands", "USD", &[]),
    ("KY", "Cayman Islands", "KYD", &[]),
    ("CW", "Curaçao", "ANG", &[]),
    ("GL", "Greenland", "DKK", &[]),
    ("GP", "Guadeloupe", "EUR", &[]),
    ("MQ", "Martinique", "EUR", &[]),
    ("MS", "Montserrat", "XCD", &[]),
    ("PR", "Puerto Rico", "USD", &[]),
    ("BL", "Saint Barthélemy", "EUR", &[]),
    ("MF", "Saint Martin", "EUR", &[]),
    ("PM", "Saint Pierre & Miquelon", "EUR", &[]),
    ("SX", "Sint Maarten", "ANG", &[]),
    ("TC", "Turks & Caicos Islands", "USD", &[]),
    ("VI", "U.S. Virgin Islands", "USD", &[]),
    ("AR", "Argentina", "ARS", &["USD"]),
    ("BO", "Bolivia", "BOB", &[]),
    ("BR", "Brazil", "BRL", &[]),
    ("CL", "Chile", "CLP", &[]),
    ("CO", "Colombia", "COP", &[]),
    ("EC", "Ecuador", "USD", &[]),
    ("GY", "Guyana", "GYD", &[]),
    ("PY", "Paraguay", "PYG", &[]),
    ("PE", "Peru", "PEN", &[]),
    ("SR", "Suriname", "SRD", &[]),
    ("UY", "Uruguay", "UYU", &[]),
    ("VE", "Venezuela", "VES", &["USD"]),
    ("FK", "Falkland Islands", "FKP", &[]),
    ("GF", "French Guiana", "EUR", &[]),
    ("GS", "South Georgia & South Sandwich Islands", "GBP", &[]),
    ("AU", "Australia", "AUD", &[]),
    ("FJ", "Fiji", "FJD", &[]),
    ("KI", "Kiribati", "AUD", &[]),
    ("MH", "Marshall Islands", "USD", &[]),
    ("FM", "Micronesia", "USD", &[]),
    ("NR", "Nauru", "AUD", &[]),
    ("NZ", "New Zealand", "NZD", &[]),
    ("PW", "Palau", "USD", &[]),
    ("PG", "Papua New Guinea", "PGK", &[]),
    ("WS", "Samoa", "WST", &[]),
    ("SB", "Solomon Islands", "SBD", &[]),
    ("TO", "Tonga", "TOP", &[]),
    ("TV", "Tuvalu", "AUD", &[]),
    ("VU", "Vanuatu", "VUV", &[]),
    ("AS", "American Samoa", "USD", &[]),
    ("CK", "Cook Islands", "NZD", &[]),
    ("PF", "French Polynesia", "XPF", &[]),
    ("GU", "Guam", "USD", &[]),
    ("NC", "New Caledonia", "XPF", &[]),
    ("NU", "Niue", "NZD", &[]),
    ("NF", "Norfolk Island", "AUD", &[]),
    ("MP", "Northern Mariana Islands", "USD", &[]),
    ("PN", "Pitcairn Islands", "NZD", &[]),
    ("TK", "Tokelau", "NZD", &[]),
    ("WF", "Wallis & Futuna", "XPF", &[]),
    ("UM", "U.S. Minor Outlying Islands", "USD", &[]),
    ("TF", "French Southern Territories", "EUR", &[]),
    ("AQ", "Antarctica", "USD", &[]),
    ("BV", "Bouvet Island", "NOK", &[]),
    ("HM", "Heard & McDonald Islands", "AUD", &[]),
];

/// The payment methods this build knows how to suggest. See the module
/// comment: absence is not a prohibition.
const PAYMENT_METHODS: &[(&str, PaymentMethodCategory, &[&str])] = &[
    (
        "Cash Deposit",
        Cash,
        &[
            "cash deposit",
            "bank deposit",
            "cash at bank",
            "deposit cash",
        ],
    ),
    (
        "Cash in Person",
        Cash,
        &[
            "cash",
            "cash in person",
            "face to face",
            "f2f",
            "meet in person",
        ],
    ),
    (
        "M-Pesa Kenya (Safaricom)",
        MobileMoney,
        &["mpesa", "m-pesa", "safaricom"],
    ),
    (
        "Mpesa Pochi la Biashara",
        MobileMoney,
        &["pochi", "pochi la biashara"],
    ),
    (
        "MTN Mobile Money",
        MobileMoney,
        &["mtn", "momo", "mtn momo"],
    ),
    ("Airtel Money", MobileMoney, &["airtel"]),
    ("Tigo Pesa", MobileMoney, &["tigo"]),
    ("Vodafone Cash", MobileMoney, &["vodafone"]),
    ("Telebirr", MobileMoney, &["telebirr ethiopia"]),
    ("GCash", MobileMoney, &["gcash philippines"]),
    ("Maya", MobileMoney, &["paymaya"]),
    ("bKash", MobileMoney, &["bkash"]),
    ("Nagad", MobileMoney, &[]),
    ("EasyPaisa", MobileMoney, &[]),
    ("JazzCash", MobileMoney, &["jazz"]),
    ("I&M Bank", BankTransfer, &["i&m", "im bank", "imb"]),
    ("Equity Bank", BankTransfer, &["equity"]),
    ("KCB", BankTransfer, &["kcb bank", "kenya commercial bank"]),
    ("Bank Transfer", BankTransfer, &["wire", "bank"]),
    ("SEPA", BankTransfer, &["sepa transfer", "iban"]),
    (
        "Faster Payments (UK)",
        BankTransfer,
        &["faster payments uk"],
    ),
    (
        "FPS (Faster Payment System)",
        BankTransfer,
        &["fps", "fps hong kong", "轉數快"],
    ),
    ("ACH", BankTransfer, &["ach transfer"]),
    ("Wire Transfer", BankTransfer, &["swift", "wire"]),
    ("PromptPay", BankTransfer, &["promptpay thailand"]),
    (
        "Interac e-Transfer",
        BankTransfer,
        &["interac", "e-transfer"],
    ),
    ("PayID", BankTransfer, &["payid australia", "osko"]),
    ("SPEI", BankTransfer, &["spei mexico"]),
    ("Taiwan Pay", BankTransfer, &["taiwan pay", "twqr"]),
    ("Toss", Fintech, &["toss korea"]),
    ("KakaoPay", Fintech, &["kakao pay", "kakao"]),
    ("BLIK", BankTransfer, &["blik poland"]),
    ("Swish", BankTransfer, &["swish sweden"]),
    ("Vipps", BankTransfer, &["vipps norway"]),
    ("MobilePay", BankTransfer, &["mobilepay denmark"]),
    ("TWINT", BankTransfer, &["twint switzerland"]),
    ("Kaspi.kz", Fintech, &["kaspi", "kaspi gold"]),
    ("Idram", Fintech, &["idram armenia"]),
    ("Payme", Fintech, &["payme uzbekistan"]),
    ("Click", Fintech, &["click uzbekistan"]),
    ("eSewa", MobileMoney, &["esewa nepal"]),
    ("Khalti", MobileMoney, &["khalti nepal"]),
    ("Wing", MobileMoney, &["wing cambodia"]),
    ("ABA Pay", BankTransfer, &["aba", "aba bank"]),
    ("QPay", Fintech, &["qpay mongolia"]),
    ("CliQ", BankTransfer, &["cliq jordan"]),
    ("Zain Cash", MobileMoney, &["zaincash"]),
    ("Fawran", BankTransfer, &["fawran qatar"]),
    ("KNET", BankTransfer, &["knet kuwait"]),
    ("BenefitPay", Fintech, &["benefit pay bahrain"]),
    ("Thawani", Fintech, &["thawani oman"]),
    ("D17", Fintech, &["d17 tunisia"]),
    ("BaridiMob", Fintech, &["baridimob algeria", "baridi"]),
    ("EcoCash", MobileMoney, &["ecocash zimbabwe"]),
    ("M-Pesa Mozambique", MobileMoney, &["mpesa mozambique"]),
    ("Orange Money", MobileMoney, &["orange"]),
    (
        "Juice by MCB",
        BankTransfer,
        &["juice mauritius", "mcb juice"],
    ),
    ("SINPE Movil", BankTransfer, &["sinpe", "sinpe movil"]),
    ("Yappy", Fintech, &["yappy panama"]),
    ("Tigo Money", MobileMoney, &["tigo"]),
    ("Lynk", Fintech, &["lynk jamaica"]),
    ("WiPay", Fintech, &["wipay"]),
    ("tPago", Fintech, &["tpago dominican"]),
    ("LankaPay", BankTransfer, &["lankapay", "ceft"]),
    ("BCEL One", BankTransfer, &["bcel", "bcel one laos"]),
    ("Revolut", Fintech, &["rev"]),
    ("Wise", Fintech, &["transferwise"]),
    ("Skrill", Fintech, &[]),
    ("PayPal", Fintech, &["pp"]),
    ("Zelle", Fintech, &[]),
    ("UPI", Fintech, &["upi india", "bhim", "gpay", "phonepe"]),
    ("PIX", Fintech, &["pix brazil"]),
    ("Alipay", Fintech, &["alipay china"]),
    ("WeChat Pay", Fintech, &["wechat"]),
    ("JKOPay", Fintech, &["jko", "jkopay", "街口支付"]),
    ("LINE Pay", Fintech, &["linepay", "line"]),
    ("PayMe", Fintech, &["payme", "payme hsbc"]),
    (
        "AlipayHK",
        Fintech,
        &["alipay hk", "alipayhk", "支付寶香港"],
    ),
    ("WeChat Pay HK", Fintech, &["wechat hk", "weixin hk"]),
    (
        "Octopus (O! ePay)",
        Fintech,
        &["octopus", "oepay", "o! epay", "八達通"],
    ),
    ("MPay", Fintech, &["mpay", "macau pass", "澳門通"]),
    ("BOC Pay", Fintech, &["boc pay", "bank of china pay"]),
    ("Mercado Pago", Fintech, &["mercadopago"]),
    ("Papara", Fintech, &[]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::MethodTable;
    use openfiat_storage::mem::MemoryStore;
    use std::collections::HashSet;

    fn fetch() -> ReferenceData {
        let mut table: MethodTable<MemoryStore> = MethodTable::new();
        register(&mut table);
        let state = NodeState::new_for_test(MemoryStore::new());
        serde_json::from_value(
            table
                .dispatch(&state, "getReferenceData", serde_json::Value::Null)
                .expect("the reference read takes no parameters and cannot fail"),
        )
        .expect("the wire form must deserialize back into the type that produced it")
    }

    /// The lesson that motivated routing every code through
    /// `FiatCurrency`: the web app shipped the Somaliland shilling as
    /// "SLSH", four letters, which that type rejects. A merchant could
    /// pick it out of the dropdown and their advertisement was refused at
    /// the node with a deserialization error naming a field they never
    /// filled in. A currency an interface offers has to be one the
    /// protocol can carry, and this is what proves it for all 159.
    #[test]
    fn every_code_in_the_tables_is_one_the_protocol_can_carry() {
        // `build` panics on a malformed code, so reaching the assertions
        // at all is already half the check.
        let data = fetch();
        assert_eq!(data.currencies.len(), CURRENCIES.len());
        for currency in &data.currencies {
            assert_eq!(
                FiatCurrency::parse(currency.code.as_str()).as_ref(),
                Ok(&currency.code),
                "a code that has already been parsed must survive a round trip"
            );
        }
    }

    /// The reason the lists come back from one method. A country naming a
    /// currency that is not in `currencies` gives an interface a code it
    /// cannot label, and it would show a bare "XAF" where every other row
    /// reads "Central African CFA franc".
    #[test]
    fn every_currency_a_country_trades_in_is_described_in_the_currency_list() {
        let data = fetch();
        let described: HashSet<&str> = data.currencies.iter().map(|c| c.code.as_str()).collect();
        for country in &data.countries {
            assert!(
                described.contains(country.currency.as_str()),
                "{} trades in {} and nothing describes it",
                country.name,
                country.currency
            );
            for alt in &country.alt_currencies {
                assert!(
                    described.contains(alt.as_str()),
                    "{} also trades in {alt} and nothing describes it",
                    country.name
                );
            }
        }
    }

    /// Country and currency codes are what a client keys off — a
    /// duplicate means one of the two rows is unreachable, and which one
    /// wins depends on whether the client built a map forwards or
    /// backwards.
    #[test]
    fn no_country_or_currency_or_payment_method_is_listed_twice() {
        let data = fetch();
        let mut country_codes = HashSet::new();
        for country in &data.countries {
            assert!(
                country_codes.insert(country.code.clone()),
                "{} is listed twice",
                country.code
            );
        }
        let mut currency_codes = HashSet::new();
        for currency in &data.currencies {
            assert!(
                currency_codes.insert(currency.code.clone()),
                "{} is listed twice",
                currency.code
            );
        }
        let mut names = HashSet::new();
        for method in &data.payment_methods {
            assert!(
                names.insert(method.name.clone()),
                "{} is listed twice",
                method.name
            );
        }
    }

    /// An alias is matched against lowercased user input, so an alias
    /// carrying an uppercase letter can never be typed into existence —
    /// it would sit in the table looking like it worked.
    #[test]
    fn payment_method_aliases_are_lowercase_because_that_is_what_they_are_matched_against() {
        for method in &fetch().payment_methods {
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

    /// The revision exists to be compared — between two nodes, and
    /// between a cached copy and a fresh read. Both uses are worthless if
    /// it is not a pure function of the data.
    #[test]
    fn the_revision_is_stable_across_calls_and_derived_only_from_the_data() {
        let first = fetch();
        let second = fetch();
        assert_eq!(first.revision, second.revision);
        assert_eq!(first.revision.len(), 16);

        // And it genuinely tracks the content: the same digest over a
        // mutated copy must differ, or a client caching on it would never
        // notice a table change.
        let mut mutated = first.clone();
        mutated.payment_methods.pop();
        let bytes = serde_json::to_vec(&(
            &mutated.currencies,
            &mutated.countries,
            &mutated.payment_methods,
            &mutated.mints,
        ))
        .unwrap();
        let recomputed: String = sha256(&bytes)
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_ne!(recomputed, first.revision);
    }

    /// Cash is the floor of what this network can do. Every other rail is
    /// a local system that may or may not exist where a user is; cash
    /// exists everywhere, and dropping it from the table would quietly
    /// make a country with no electronic rails untradeable.
    #[test]
    fn cash_is_always_offered_because_it_is_the_only_universal_rail() {
        let data = fetch();
        let cash: Vec<&PaymentMethod> = data
            .payment_methods
            .iter()
            .filter(|m| m.category == PaymentMethodCategory::Cash)
            .collect();
        assert!(
            cash.iter().any(|m| m.name == "Cash Deposit"),
            "OFS-2100 §13 names Cash Deposit specifically"
        );
        assert!(
            cash.iter().any(|m| m.name == "Cash in Person"),
            "a deposit leaves a trail an arbitrator can read and a hand-off does not; \
             they are different risks and cannot be one entry"
        );
    }

    /// A method whose name a client cannot store is not a method. Names
    /// go onto advertisements verbatim and are compared verbatim, so
    /// stray whitespace would produce two methods that look identical.
    #[test]
    fn payment_method_names_are_stored_exactly_as_they_are_compared() {
        for method in &fetch().payment_methods {
            assert_eq!(method.name.trim(), method.name, "{:?}", method.name);
            assert!(!method.name.is_empty());
        }
    }

    /// Two different national payment systems share the abbreviation
    /// "FPS": Hong Kong's Faster Payment System and the UK's Faster
    /// Payments. A merchant in either place who typed "fps" and was
    /// handed the other country's rail would be advertising a system
    /// they cannot receive on, so the UK entry deliberately does not
    /// claim the bare alias.
    #[test]
    fn the_two_faster_payment_systems_do_not_answer_to_each_others_names() {
        let data = fetch();
        let aliases_of = |name: &str| {
            data.payment_methods
                .iter()
                .find(|m| m.name == name)
                .unwrap_or_else(|| panic!("{name} must be listed"))
                .aliases
                .clone()
        };
        assert!(aliases_of("FPS (Faster Payment System)").contains(&"fps".to_string()));
        assert!(
            !aliases_of("Faster Payments (UK)").contains(&"fps".to_string()),
            "the UK entry must not answer to Hong Kong's abbreviation"
        );

        // The same rule, for the wallets that share a brand across a
        // border they cannot pay across: an AlipayHK account cannot
        // receive from a mainland Alipay one.
        for separately_listed in ["AlipayHK", "Alipay", "WeChat Pay HK", "WeChat Pay"] {
            assert!(
                data.payment_methods
                    .iter()
                    .any(|m| m.name == separately_listed),
                "{separately_listed} must be selectable on its own"
            );
        }
    }

    /// The mints are relayed, not restated. A second transcription in
    /// this file could drift from `openfiat_chain::mints`, and then a
    /// client building a routing table from this method and a node
    /// rendering an advertisement would put two different names on the
    /// same address.
    #[test]
    fn the_mints_are_exactly_what_this_build_resolves_advertisements_against() {
        let data = fetch();
        assert_eq!(data.mints.len(), openfiat_chain::mints::KNOWN_MINTS.len());
        for (sent, known) in data.mints.iter().zip(openfiat_chain::mints::KNOWN_MINTS) {
            assert_eq!(sent.mint, known.mint);
            assert_eq!(sent.symbol, known.symbol);
            assert_eq!(sent.decimals, known.decimals);
        }
    }

    /// The concrete failure that put mints on this method. An interface
    /// routing on a hand-written `"SOL"` matched nothing, because the
    /// mint this network settles wrapped SOL through is named `wSOL` —
    /// and a market page that can never match an advertisement looks
    /// exactly like a market with no advertisements.
    #[test]
    fn the_wrapped_sol_mint_is_named_wsol_and_scaled_by_nine() {
        let data = fetch();
        let wsol = data
            .mints
            .iter()
            .find(|m| m.mint == "So11111111111111111111111111111111111111112")
            .expect("wrapped SOL is on the shipped settlement list");
        assert_eq!(wsol.symbol, "wSOL");
        // The other reason decimals travel beside the symbol: every
        // other mint here is 6, and a client that assumed 6 would print
        // every SOL amount a thousand times too large.
        assert_eq!(wsol.decimals, 9);
    }

    /// An address is the identity and a symbol is a nickname. Two mints
    /// sharing a symbol is legitimate — it is what a look-alike token
    /// *is* — so nothing may key off one; a duplicate address, though,
    /// means one of the two rows can never be looked up.
    #[test]
    fn a_mint_is_identified_by_its_address_and_the_addresses_are_unique() {
        let data = fetch();
        let mut addresses = HashSet::new();
        for mint in &data.mints {
            assert!(
                addresses.insert(mint.mint.clone()),
                "{} is listed twice",
                mint.mint
            );
            assert!(
                !mint.symbol.is_empty(),
                "{} has no name to offer",
                mint.mint
            );
        }
    }
}
