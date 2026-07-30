//! A devnet FX oracle that publishes rates it actually looked up.
//!
//! Run it once and the cluster has a live feed until the records expire.
//! Run it on a timer and it stays live.
//!
//! ```text
//! cargo run -p openfiat-cli --example devnet_oracle -- \
//!     --node https://openfiat.allenhark.com \
//!     --identity ~/devnet-oracle.json
//! ```
//!
//! # Why this exists, and what it must never become
//!
//! An oracle record carries an `expires_at` on purpose: a feed that stops
//! is supposed to go stale rather than serve a frozen number, and
//! `median_exchange_rate` filters expired records so a floating
//! advertisement becomes unpriceable rather than mispriced. That is
//! correct, and it is why devnet's feed died — nothing was republishing.
//!
//! The obvious fix is a cron job publishing a constant. That would be
//! *worse than the dead feed*: every floating advertisement would price
//! confidently off a number nobody measured, and the failure would be
//! invisible because everything would look healthy. So this looks the
//! rates up, names its sources in what it publishes, and refuses to
//! publish at all when it cannot reach them (see [`Rates::fetch`]).
//!
//! # Where the numbers come from
//!
//! Two real sources, composed, because neither answers the question on
//! its own:
//!
//! - **exchangerate-api.com** (`open.er-api.com`, free tier, no key) for
//!   USD→KES and USD→NGN. Updated daily.
//! - **CoinGecko** for USDC→USD and USDT→USD.
//!
//! The second is not ceremony. USDC is *approximately* a dollar, and
//! publishing `USDC/KES = USD/KES` would be quietly asserting a peg this
//! process never checked — the same class of fiction as the hardcoded
//! cron, just harder to notice. Measuring both legs means a depegged
//! stablecoin shows up in the rate instead of being assumed away.
//!
//! # This is a devnet tool
//!
//! A real oracle provider stakes, is challenged, and answers for what it
//! publishes. This is a keypair on someone's laptop reading two free
//! APIs. It is honest about *where its numbers come from*, which is the
//! part that matters for a devnet; it is not a production feed and the
//! service it registers says so.

use openfiat_crypto::Keypair;
use openfiat_oracles::OracleId;
use openfiat_oracles::events::{OraclePublish, SignedOraclePublish};
use openfiat_oracles::record::OracleData;
use openfiat_registry::{Registration, SignedRegistration};
use openfiat_types::{MarketDataService, ServiceId, ServiceType, Timestamp};
use std::collections::HashMap;

/// How long a published record stays current.
///
/// Three hours, against a fiat leg that updates daily and a stablecoin
/// leg that moves continuously. The window is deliberately shorter than
/// the fiat source's own cycle: it means a publisher that dies is noticed
/// within hours rather than a day, and the cost of that is republishing
/// the same fiat number with a fresher stablecoin leg, which is new
/// information rather than a laundered old one.
const RECORD_TTL_SECS: u64 = 3 * 60 * 60;

/// How stale the fiat source may be before this refuses to publish.
///
/// The source reports when it last updated. If that is two days ago the
/// source itself has stalled, and republishing its number under a fresh
/// `expires_at` would be presenting stale data as current — precisely
/// what the expiry mechanism exists to prevent.
const MAX_SOURCE_AGE_SECS: u64 = 48 * 60 * 60;

const FIAT_SOURCE: &str = "https://open.er-api.com/v6/latest/USD";
const STABLECOIN_SOURCE: &str =
    "https://api.coingecko.com/api/v3/simple/price?ids=usd-coin,tether&vs_currencies=usd";

/// The pairs devnet advertises in. Each is published separately, because
/// a consumer looks up one pair and a bundle would make it parse others.
const PAIRS: [(&str, &str); 4] = [
    ("USDC", "KES"),
    ("USDT", "KES"),
    ("USDC", "NGN"),
    ("USDT", "NGN"),
];

struct Rates {
    /// USD per unit of each stablecoin, measured rather than assumed.
    stablecoin_usd: HashMap<String, f64>,
    /// Units of each fiat per USD.
    fiat_per_usd: HashMap<String, f64>,
    fiat_updated_at: u64,
}

impl Rates {
    /// Reads both sources, or fails.
    ///
    /// Every failure here is fatal rather than recoverable-with-a-default.
    /// A default would be a number this process invented, and the whole
    /// point is that it does not invent numbers.
    async fn fetch(http: &reqwest::Client) -> Result<Self, Box<dyn std::error::Error>> {
        let fiat: serde_json::Value = http.get(FIAT_SOURCE).send().await?.json().await?;
        if fiat["result"] != "success" {
            return Err(format!("fiat source did not succeed: {}", fiat["result"]).into());
        }
        let fiat_updated_at = fiat["time_last_update_unix"]
            .as_u64()
            .ok_or("fiat source gave no update time")?;

        let age = now_secs().saturating_sub(fiat_updated_at);
        if age > MAX_SOURCE_AGE_SECS {
            return Err(format!(
                "fiat source last updated {}h ago, past the {}h limit — publishing it now \
                 would present stale data as current",
                age / 3600,
                MAX_SOURCE_AGE_SECS / 3600
            )
            .into());
        }

        let mut fiat_per_usd = HashMap::new();
        for (_, quote) in PAIRS {
            let rate = fiat["rates"][quote]
                .as_f64()
                .ok_or_else(|| format!("fiat source has no rate for {quote}"))?;
            fiat_per_usd.insert(quote.to_string(), rate);
        }

        // Read as text first so a rate-limit page or an error body ends
        // up in the message. Parsing straight to a `Value` would report
        // only "no USD price for USDC", which says nothing about why.
        let body = http.get(STABLECOIN_SOURCE).send().await?.text().await?;
        let coins: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| format!("stablecoin source returned {body:.200} ({e})"))?;
        let mut stablecoin_usd = HashMap::new();
        for (id, ticker) in [("usd-coin", "USDC"), ("tether", "USDT")] {
            let price = coins[id]["usd"]
                .as_f64()
                .ok_or_else(|| format!("no USD price for {ticker} in {body:.200}"))?;
            stablecoin_usd.insert(ticker.to_string(), price);
        }

        Ok(Self {
            stablecoin_usd,
            fiat_per_usd,
            fiat_updated_at,
        })
    }

    fn pair(&self, base: &str, quote: &str) -> Option<f64> {
        Some(self.stablecoin_usd.get(base)? * self.fiat_per_usd.get(quote)?)
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs()
}

/// Loads the signing identity, or creates one and says where it went.
///
/// Persisted rather than generated per run because a new keypair each
/// time would register a new provider each time, and the registry would
/// fill with one-shot oracles that never publish again.
fn load_identity(path: &str) -> Result<Keypair, Box<dyn std::error::Error>> {
    let expanded = shellexpand(path);
    if let Ok(bytes) = std::fs::read(&expanded) {
        let seed: Vec<u8> = serde_json::from_slice(&bytes)?;
        let seed: [u8; 32] = seed
            .get(..32)
            .and_then(|s| s.try_into().ok())
            .ok_or("identity file is not a 32-byte seed")?;
        return Ok(Keypair::from_seed(seed));
    }
    let keypair = Keypair::generate();
    std::fs::write(&expanded, serde_json::to_vec(&keypair.seed().to_vec())?)?;
    eprintln!("created a new oracle identity at {expanded}");
    Ok(keypair)
}

fn shellexpand(path: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => path.to_string(),
    }
}

async fn send(
    http: &reqwest::Client,
    node: &str,
    method: &str,
    payload: &[u8],
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use base64::Engine;
    let response: serde_json::Value = http
        .post(format!("{}/rpc", node.trim_end_matches('/')))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": { "data": base64::engine::general_purpose::STANDARD.encode(payload) },
        }))
        .send()
        .await?
        .json()
        .await?;
    if let Some(error) = response.get("error") {
        return Err(format!("{method} failed: {error}").into());
    }
    Ok(response["result"].clone())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str, default: &str| -> String {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| default.to_string())
    };
    let node = flag("--node", "http://localhost:7080");
    let identity_path = flag("--identity", "./devnet-oracle.json");

    let keypair = load_identity(&identity_path)?;
    let provider = openfiat_network::identity::peer_id_from_public_key(&keypair.public_key())
        .expect("a keypair's public key always derives a peer id");
    // A descriptive User-Agent is not politeness here: CoinGecko answers
    // 403 without one, and reqwest sends none by default. Saying who is
    // calling is also the right thing when reading someone's free tier.
    let http = reqwest::Client::builder()
        .user_agent(concat!(
            "openfiat-devnet-oracle/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()?;

    let rates = Rates::fetch(&http).await?;
    println!(
        "fiat leg from open.er-api.com, updated {}h ago",
        now_secs().saturating_sub(rates.fiat_updated_at) / 3600
    );

    // Registered every run rather than once. A registration expires when
    // its provider stops sending health updates, so a publisher that
    // comes back after an outage has to re-announce itself — and
    // re-registering an existing service is idempotent.
    let registration = Registration {
        service_id: ServiceId::new("devnet-fx-oracle"),
        service_type: ServiceType::MarketData(MarketDataService::FxOracle),
        provider: provider.clone(),
        provider_public_key: keypair.public_key(),
        // Gossip-reachable only. An FX oracle publishes; nothing dials it,
        // so declaring an endpoint would be declaring a door that is not
        // there.
        endpoints: Vec::new(),
        supported_ofs: vec![1500, 7000],
        region: None,
        // Says what it is, in the one field a consumer actually reads. A
        // devnet feed presenting itself as a production oracle is the
        // failure this whole tool is written to avoid.
        capabilities: PAIRS
            .iter()
            .map(|(base, quote)| format!("{base}/{quote}"))
            .chain([
                "devnet".to_string(),
                "source:open.er-api.com+coingecko".to_string(),
            ])
            .collect(),
        pricing: None,
        payout_wallet: None,
        timestamp: Timestamp::now(),
    };
    let signed = SignedRegistration::sign(registration, &keypair);
    send(
        &http,
        &node,
        "sendProviderRegister",
        &openfiat_serialization::json::to_bytes(&signed)?,
    )
    .await?;
    println!("registered devnet-fx-oracle");

    let now = Timestamp::now();
    for (base, quote) in PAIRS {
        let Some(rate) = rates.pair(base, quote) else {
            eprintln!("no rate for {base}/{quote}, skipping rather than inventing one");
            continue;
        };

        let publish = OraclePublish {
            // Stable per pair, so republishing updates the record a
            // consumer is already reading rather than accumulating a new
            // one every run.
            id: OracleId::new(format!(
                "devnet-{}-{}",
                base.to_lowercase(),
                quote.to_lowercase()
            )),
            provider: provider.clone(),
            provider_public_key: keypair.public_key(),
            data: OracleData::ExchangeRate {
                base: base.to_string(),
                quote: quote.to_string(),
                rate,
            },
            version: now.as_millis(),
            timestamp: now,
            expires_at: Timestamp::from_millis(now.as_millis() + RECORD_TTL_SECS * 1000),
        };
        let signed = SignedOraclePublish::sign(publish, &keypair);
        send(
            &http,
            &node,
            "sendOraclePublish",
            &openfiat_serialization::json::to_bytes(&signed)?,
        )
        .await?;
        println!("published {base}/{quote} = {rate:.4}");
    }

    Ok(())
}
