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
//!
//! # What "covered" means, and where the count is checked
//!
//! Half the country table used to name no rail at all. A merchant in
//! Jakarta, Ho Chi Minh City, Riyadh or Caracas opened a picker, found
//! cash, two generic transfers and four global fintechs, and had no way to
//! tell that apart from a network that does not reach them. The rails were
//! not missing because they do not exist — GoPay, VietQR, STC Pay and Pago
//! Móvil are how those four places are paid — they were missing because
//! nobody had written them down.
//!
//! `openfiat_rpc::methods::reference`'s
//! `every_country_resolves_to_a_local_rail_or_is_on_the_reviewed_list`
//! is where that is now counted, because it is the only place both this
//! table and the country table are visible. A country with no rail must
//! appear on that test's excused list with a reason, and a country that
//! gains one must come off it.
//!
//! The bar for adding a row is that the scheme is real, current, and one a
//! consumer can be paid over person-to-person. Card-acceptance networks
//! and merchant-checkout schemes are not that, however national they are,
//! and a rail this table is unsure of is left out and recorded as a gap
//! rather than guessed. The failure a guess produces is not a cosmetic
//! one: a merchant selects it, a buyer cannot pay it, and the trade dies
//! at the moment money should have moved.

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
///
/// The territories are in it for the same reason the UK is: the EPC's own
/// scope list names them, and a merchant in Réunion or Martinique holds a
/// French IBAN that a SEPA credit transfer reaches exactly as it reaches
/// one in Lyon. Leaving them out was not a neutral omission — it made nine
/// inhabited places look like they had no bank rail at all. Åland is here
/// for the same reason under Finland, and Guernsey, Jersey, the Isle of
/// Man and Gibraltar were already.
///
/// The French Pacific territories (New Caledonia, French Polynesia,
/// Wallis and Futuna) are deliberately *not* here. They are outside the
/// scope list, and they settle in CFP francs rather than euro.
const SEPA: &[&str] = &[
    "AD", "AT", "AX", "BE", "BG", "BL", "CH", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FR", "GB",
    "GF", "GG", "GI", "GP", "GR", "HR", "HU", "IE", "IM", "IS", "IT", "JE", "LI", "LT", "LU", "LV",
    "MC", "MF", "MQ", "MT", "NL", "NO", "PL", "PM", "PT", "RE", "RO", "SE", "SI", "SK", "SM", "VA",
    "YT",
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
    // Egypt only. Vodafone Ghana became Telecel Ghana and the wallet
    // became Telecel Cash, so listing Ghana here would offer a merchant a
    // brand that no longer answers to the name — see `telecel-cash`.
    (
        "vodafone-cash",
        "Vodafone Cash",
        MobileMoney,
        &["vodafone"],
        Some(&["EG"]),
    ),
    (
        "telecel-cash",
        "Telecel Cash",
        MobileMoney,
        &["telecel", "telecel ghana"],
        Some(&["GH"]),
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
    // The rest of M-Pesa. One row per operating company rather than one
    // row for the brand, because the accounts do not interoperate: a
    // Tanzanian M-Pesa number cannot be paid from a Kenyan M-Pesa
    // account, and a merchant offered "M-Pesa" in Dar es Salaam would be
    // advertising a rail their buyer cannot reach. None of them carries
    // the bare `mpesa` alias — that belongs to Kenya, where the brand
    // started and where an unqualified "M-Pesa" means one thing.
    (
        "mpesa-tanzania",
        "M-Pesa Tanzania",
        MobileMoney,
        &["mpesa tanzania", "vodacom tanzania"],
        Some(&["TZ"]),
    ),
    (
        "mpesa-drc",
        "M-Pesa DRC",
        MobileMoney,
        &["mpesa congo", "vodacom congo"],
        Some(&["CD"]),
    ),
    (
        "mpesa-lesotho",
        "M-Pesa Lesotho",
        MobileMoney,
        &["mpesa lesotho"],
        Some(&["LS"]),
    ),
    (
        "mpesa-ethiopia",
        "M-Pesa Ethiopia",
        MobileMoney,
        &["mpesa ethiopia", "safaricom ethiopia"],
        Some(&["ET"]),
    ),
    (
        "cbe-birr",
        "CBE Birr",
        MobileMoney,
        &["cbebirr", "commercial bank of ethiopia"],
        Some(&["ET"]),
    ),
    // West Africa. Bank transfer is the unusual rail across most of this
    // list; these are how people are actually paid.
    (
        "wave-money",
        "Wave Mobile Money",
        MobileMoney,
        // Not the bare "wave": Myanmar has an unrelated operator trading
        // under that word, so an unqualified alias would resolve to
        // whichever of the two this table happened to list first.
        &["wave africa", "wave senegal"],
        Some(&["BF", "CI", "GM", "ML", "SN", "UG"]),
    ),
    (
        "moov-money",
        "Moov Money",
        MobileMoney,
        &["moov", "moov africa", "flooz"],
        Some(&["BF", "BJ", "CF", "CI", "GA", "ML", "NE", "TD", "TG"]),
    ),
    // Horn of Africa. Somaliland is `XS` here, the pseudo-code
    // `openfiat_rpc`'s country table gives it; ZAAD is Telesom's and does
    // not serve the south, where EVC Plus does.
    (
        "evc-plus",
        "EVC Plus",
        MobileMoney,
        &["evcplus", "hormuud"],
        Some(&["SO"]),
    ),
    (
        "zaad",
        "ZAAD",
        MobileMoney,
        &["telesom zaad"],
        Some(&["XS"]),
    ),
    (
        "bankily",
        "Bankily",
        MobileMoney,
        &["bankily mauritania"],
        Some(&["MR"]),
    ),
    // The Americas and the Pacific.
    (
        "moncash",
        "MonCash",
        MobileMoney,
        &["digicel moncash"],
        Some(&["HT"]),
    ),
    (
        "transfermovil",
        "Transfermóvil",
        MobileMoney,
        &["transfermovil", "etecsa"],
        Some(&["CU"]),
    ),
    (
        "mpaisa",
        "M-PAiSA",
        MobileMoney,
        &["mpaisa fiji", "vodafone fiji"],
        Some(&["FJ"]),
    ),
    // South Asia.
    (
        "rocket-dbbl",
        "Rocket (Dutch-Bangla)",
        MobileMoney,
        &["rocket bangladesh", "dutch bangla"],
        Some(&["BD"]),
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
    // The territories are on the ACH network, not an approximation of it:
    // a bank in San Juan or Hagåtña routes through the same operators as
    // one in Ohio. Leaving them off made five inhabited places that use
    // the dollar and US banks look like they had no bank rail.
    (
        "ach",
        "ACH",
        BankTransfer,
        &["ach transfer"],
        Some(&["AS", "GU", "MP", "PR", "US", "VI"]),
    ),
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
        // The Indian Ocean and Norfolk territories bank with Australian
        // banks on the Australian dollar, so the New Payments Platform
        // reaches them.
        Some(&["AU", "CC", "CX", "NF"]),
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
        // Svalbard is Norway for banking purposes; Bouvet Island has no
        // population and is not listed.
        Some(&["NO", "SJ"]),
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
    // National instant-payment schemes. Each of these is the rail a person
    // in that country means when they say "send it to my phone" — the same
    // role PIX plays in Brazil — and each is reachable by a consumer for a
    // person-to-person transfer, which is the test for being in this table
    // at all. Card-acceptance networks and merchant-checkout schemes are
    // not, however national.
    (
        "payshap",
        "PayShap",
        BankTransfer,
        &["payshap south africa", "shapid"],
        Some(&["ZA"]),
    ),
    (
        "fnb-ewallet",
        "FNB eWallet",
        BankTransfer,
        &["fnb ewallet"],
        Some(&["NA", "ZA"]),
    ),
    (
        "instapay-eg",
        "InstaPay (Egypt)",
        BankTransfer,
        // Never the bare "instapay": the Philippines runs an unrelated
        // scheme of the same name, and a merchant handed the wrong
        // country's would be advertising one they cannot receive on. The
        // same reasoning the UK and Hong Kong Faster Payments rows use.
        &["instapay egypt"],
        Some(&["EG"]),
    ),
    (
        "instapay-ph",
        "InstaPay (Philippines)",
        BankTransfer,
        &["instapay philippines"],
        Some(&["PH"]),
    ),
    (
        "multicaixa-express",
        "Multicaixa Express",
        BankTransfer,
        &["multicaixa", "emis angola"],
        Some(&["AO"]),
    ),
    (
        "raast",
        "Raast",
        BankTransfer,
        &["raast pakistan"],
        Some(&["PK"]),
    ),
    ("imps", "IMPS", BankTransfer, &["imps india"], Some(&["IN"])),
    (
        "bi-fast",
        "BI-FAST",
        BankTransfer,
        &["bifast", "bi fast indonesia"],
        Some(&["ID"]),
    ),
    (
        "duitnow",
        "DuitNow",
        BankTransfer,
        &["duitnow malaysia"],
        Some(&["MY"]),
    ),
    (
        "paynow-sg",
        "PayNow (Singapore)",
        BankTransfer,
        &["paynow singapore"],
        Some(&["SG"]),
    ),
    (
        "vietqr",
        "VietQR",
        BankTransfer,
        &["vietqr vietnam", "napas"],
        Some(&["VN"]),
    ),
    (
        "bakong",
        "Bakong",
        BankTransfer,
        &["bakong cambodia"],
        Some(&["KH"]),
    ),
    (
        "sbp-russia",
        "SBP (Fast Payments System)",
        BankTransfer,
        &["sbp", "sbp russia"],
        Some(&["RU"]),
    ),
    (
        "privat24",
        "Privat24",
        BankTransfer,
        &["privatbank"],
        Some(&["UA"]),
    ),
    (
        "fast-turkey",
        "FAST (Turkey)",
        BankTransfer,
        &["fast turkey", "kolas"],
        Some(&["TR"]),
    ),
    ("aani", "Aani", BankTransfer, &["aani uae"], Some(&["AE"])),
    (
        "shetab",
        "Shetab",
        BankTransfer,
        &["shetab iran", "card to card"],
        Some(&["IR"]),
    ),
    (
        "bizum",
        "Bizum",
        BankTransfer,
        &["bizum spain"],
        Some(&["ES"]),
    ),
    ("mb-way", "MB WAY", BankTransfer, &["mbway"], Some(&["PT"])),
    (
        "wero",
        "Wero",
        BankTransfer,
        &["wero wallet"],
        Some(&["BE", "DE", "FR"]),
    ),
    (
        "iris-greece",
        "IRIS (Greece)",
        BankTransfer,
        &["iris greece", "dias iris"],
        Some(&["GR"]),
    ),
    (
        "ips-serbia",
        "IPS (Serbia)",
        BankTransfer,
        &["ips serbia"],
        Some(&["RS"]),
    ),
    (
        "pago-movil",
        "Pago Móvil",
        BankTransfer,
        &["pago movil", "pagomovil"],
        Some(&["VE"]),
    ),
    (
        "cuenta-rut",
        "Cuenta RUT (BancoEstado)",
        BankTransfer,
        &["cuenta rut", "bancoestado"],
        Some(&["CL"]),
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
    // Nigeria. The dominant consumer rails here are wallets and neobanks
    // rather than the telcos, which is the opposite of the pattern one
    // country west, and is why the mobile-money rows above are not enough
    // for the largest market on the continent.
    ("opay", "OPay", Fintech, &["opay nigeria"], Some(&["NG"])),
    ("palmpay", "PalmPay", Fintech, &[], Some(&["NG"])),
    ("kuda", "Kuda", Fintech, &["kuda bank"], Some(&["NG"])),
    ("moniepoint", "Moniepoint", Fintech, &[], Some(&["NG"])),
    // South-east Asia.
    (
        "gopay",
        "GoPay",
        Fintech,
        &["gopay indonesia", "gojek"],
        Some(&["ID"]),
    ),
    ("ovo", "OVO", Fintech, &["ovo indonesia"], Some(&["ID"])),
    ("dana", "DANA", Fintech, &["dana indonesia"], Some(&["ID"])),
    (
        "momo-vietnam",
        "MoMo (Vietnam)",
        Fintech,
        // Not the bare "momo": MTN's mobile money answers to that across
        // sixteen African countries, and the two are unrelated companies.
        &["momo vietnam", "momo wallet"],
        Some(&["VN"]),
    ),
    ("zalopay", "ZaloPay", Fintech, &["zalo pay"], Some(&["VN"])),
    (
        "truemoney",
        "TrueMoney Wallet",
        Fintech,
        &["truemoney", "true money"],
        Some(&["KH", "PH", "TH"]),
    ),
    (
        "tng-ewallet",
        "Touch 'n Go eWallet",
        Fintech,
        &["tng", "touch n go"],
        Some(&["MY"]),
    ),
    ("kbzpay", "KBZPay", Fintech, &["kbz pay"], Some(&["MM"])),
    (
        "paypay",
        "PayPay",
        Fintech,
        &["paypay japan"],
        Some(&["JP"]),
    ),
    // Eastern Europe, the Caucasus and the Levant.
    ("monobank", "monobank", Fintech, &["mono"], Some(&["UA"])),
    ("m10", "m10", Fintech, &["m10 azerbaijan"], Some(&["AZ"])),
    (
        "bit-israel",
        "Bit (Israel)",
        Fintech,
        &["bit israel"],
        Some(&["IL"]),
    ),
    (
        "paybox-israel",
        "PayBox (Israel)",
        Fintech,
        &["paybox israel"],
        Some(&["IL"]),
    ),
    (
        "whish-money",
        "Whish Money",
        Fintech,
        &["whish"],
        Some(&["LB"]),
    ),
    ("stc-pay", "STC Pay", Fintech, &["stcpay"], Some(&["SA"])),
    // Western Europe, beside SEPA rather than instead of it: a euro
    // transfer reaches any of these countries, and none of it is what a
    // person there hands a friend a phone number for.
    ("lydia", "Lydia", Fintech, &["lydia france"], Some(&["FR"])),
    (
        "satispay",
        "Satispay",
        Fintech,
        &["satispay italy"],
        Some(&["IT"]),
    ),
    (
        "tikkie",
        "Tikkie",
        Fintech,
        &["tikkie netherlands"],
        Some(&["NL"]),
    ),
    // The Americas.
    ("venmo", "Venmo", Fintech, &[], Some(&["US"])),
    ("cash-app", "Cash App", Fintech, &["cashapp"], Some(&["US"])),
    (
        "nequi",
        "Nequi",
        Fintech,
        &["nequi colombia"],
        Some(&["CO"]),
    ),
    (
        "daviplata",
        "Daviplata",
        Fintech,
        &["davivienda"],
        Some(&["CO"]),
    ),
    (
        "yape",
        "Yape",
        Fintech,
        &["yape peru", "bcp yape"],
        Some(&["PE"]),
    ),
    ("plin", "Plin", Fintech, &["plin peru"], Some(&["PE"])),
    (
        "uala",
        "Ualá",
        Fintech,
        &["uala argentina"],
        Some(&["AR", "CO", "MX"]),
    ),
    ("enzona", "EnZona", Fintech, &["enzona cuba"], Some(&["CU"])),
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
            // The markets this table used to answer with nothing local.
            ("ID", "GoPay"),
            ("VN", "VietQR"),
            ("MY", "DuitNow"),
            ("SG", "PayNow (Singapore)"),
            ("RU", "SBP (Fast Payments System)"),
            ("UA", "Privat24"),
            ("SA", "STC Pay"),
            ("AE", "Aani"),
            ("IL", "Bit (Israel)"),
            ("VE", "Pago Móvil"),
            ("HT", "MonCash"),
            ("SO", "EVC Plus"),
            ("AO", "Multicaixa Express"),
            ("LS", "M-Pesa Lesotho"),
            ("TG", "Moov Money"),
            // A French overseas department reaches a French IBAN over
            // SEPA exactly as Lyon does; leaving the territories out of
            // the scope list made nine inhabited places look unbanked.
            ("RE", "SEPA"),
            ("MQ", "SEPA"),
            // And the depth cases: one rail was all these had.
            ("ZA", "PayShap"),
            ("TZ", "M-Pesa Tanzania"),
            ("ET", "M-Pesa Ethiopia"),
            ("EG", "InstaPay (Egypt)"),
            ("PK", "Raast"),
            ("MX", "SPEI"),
            ("CO", "Nequi"),
            ("PE", "Yape"),
            ("CL", "Cuenta RUT (BancoEstado)"),
            ("AR", "Ualá"),
            ("US", "Venmo"),
            ("GB", "Faster Payments (UK)"),
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

    /// Four brand names are used by unrelated operators in unrelated
    /// countries, and the skeleton check catches the collision only if
    /// somebody writes the ambiguous spelling down. It cannot catch the
    /// quieter mistake: giving one of the two the *unqualified* alias, so
    /// that a merchant typing the word reaches whichever row this table
    /// happens to list first — and advertises a rail their buyer cannot
    /// reach.
    ///
    /// So the rule is that where two operators share a word, neither
    /// holds it alone. `fps` belongs to Hong Kong and the UK row is
    /// spelled out; `mpesa` belongs to Kenya, where the brand began, and
    /// every other operating company is qualified; `momo` belongs to
    /// MTN's sixteen African markets and Vietnam's wallet is qualified;
    /// `instapay` belongs to neither Egypt nor the Philippines, and
    /// `wave` to neither West Africa nor Myanmar.
    #[test]
    fn a_word_two_operators_answer_to_is_never_one_operators_alone() {
        for ambiguous in ["mpesa", "momo", "instapay", "wave", "fps"] {
            let claimants: Vec<&str> = catalog()
                .iter()
                .filter(|method| method.aliases.iter().any(|alias| alias == ambiguous))
                .map(|method| method.name.as_str())
                .collect();
            assert!(
                claimants.len() <= 1,
                "{ambiguous:?} is claimed by {claimants:?}, so typing it reaches \
                 whichever of them a client indexed first"
            );
            if let [only] = claimants[..] {
                assert!(
                    matches!(
                        only,
                        "M-Pesa Kenya (Safaricom)"
                            | "MTN Mobile Money"
                            | "FPS (Faster Payment System)"
                    ),
                    "{ambiguous:?} is held by {only:?}, which is not the operator \
                     this table decided the unqualified word means"
                );
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
