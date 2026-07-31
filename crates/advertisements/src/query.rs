//! Narrowing the order book, and reading it a page at a time.
//!
//! # Why this is not a frontend concern
//!
//! `getAdvertisements` returned every advertisement on the network, with
//! no parameters. That works at devnet volume and fails in both
//! directions at any real one: the response grows without bound, and a
//! buyer cannot find the offer they want. Filtering in the client would
//! move the second problem and leave the first — the node would still
//! serialize every advertisement on every request, and every client would
//! still download the whole book to show a page of it.
//!
//! So the narrowing happens here, in the crate that owns the records.
//!
//! # Why the cursor is an id and not an offset
//!
//! An offset silently skips rows. Advertisements are published
//! continuously; between a reader's first page and their second, a new
//! one can land ahead of both, and `skip(20).take(20)` then returns rows
//! 20-39 of a list whose contents have shifted underneath it — so one
//! advertisement is shown twice and another never at all. A reader
//! scrolling an order book has no way to notice.
//!
//! A cursor names the last row actually seen and asks for what comes
//! after it, under a total order that does not change when something is
//! inserted. That is stable by construction rather than by luck, and it
//! costs nothing extra: the id is already in the response.

use crate::record::{Advertisement, AdvertisementId, AdvertisementStatus, Direction};
use openfiat_crypto::MintAddress;
use openfiat_types::{Amount, FiatCurrency, PeerId};

/// The most advertisements one response will carry.
///
/// A caller may ask for fewer and cannot ask for more. Without a ceiling
/// the page size is chosen by whoever is calling, which makes "return
/// everything" available again under a different name.
pub const MAX_PAGE: usize = 100;

/// The page size a caller gets by asking for nothing.
pub const DEFAULT_PAGE: usize = 25;

/// What a trader actually chooses by.
///
/// Every field is optional and absent means "no constraint", so the empty
/// filter is the whole book — which keeps the unparameterised call that
/// existed before this working, one page at a time.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdvertisementFilter {
    /// The token being traded, by mint address. Not a ticker: see
    /// [`openfiat_crypto::mint`] for why an advertisement names an
    /// identity rather than a label a merchant chose.
    #[serde(default)]
    pub asset_mint: Option<MintAddress>,
    #[serde(default)]
    pub fiat_currency: Option<FiatCurrency>,
    #[serde(default)]
    pub direction: Option<Direction>,
    /// A payment method the merchant accepts. Matches if the
    /// advertisement lists it among possibly several.
    #[serde(default)]
    pub payment_method: Option<String>,
    /// Only advertisements that could take a trade of this size — that
    /// is, `min_trade <= amount <= max_trade` and enough liquidity left.
    ///
    /// A buyer with 50 USDC does not want to read about offers starting
    /// at 500, and finding that out by opening each one is the search
    /// this filter exists to remove.
    #[serde(default)]
    pub amount: Option<Amount>,
    /// Whose advertisements, by merchant PeerId.
    ///
    /// A merchant reviewing their own book asks a different question from
    /// a buyer reading the market, and it is the same question every
    /// interface with a merchant console asks. Without it a client reads
    /// the whole book and keeps the rows whose merchant matches — which
    /// works, and means the node serializes every advertisement on the
    /// network so a browser can throw nearly all of them away.
    #[serde(default)]
    pub merchant: Option<PeerId>,
    /// Which states count. Absent means active only.
    ///
    /// A disabled or deleted advertisement cannot be traded against, so
    /// returning one by default would be offering something that is not
    /// on offer.
    ///
    /// A *set* rather than one status, because the caller that needs
    /// something other than the default needs several: a merchant's
    /// console has to show a paused advertisement — it is the only screen
    /// that can put it back — alongside the live ones. With a single
    /// value that screen had to ask four times and stitch the answers
    /// together, which is the same "filter it in the client" shape one
    /// level down.
    #[serde(default)]
    pub statuses: Option<Vec<AdvertisementStatus>>,
}

/// Where to resume from, and how much to take.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Page {
    /// The id of the last advertisement the caller already has. Absent
    /// starts at the beginning.
    #[serde(default)]
    pub after: Option<AdvertisementId>,
    /// Clamped to [`MAX_PAGE`]; absent means [`DEFAULT_PAGE`].
    #[serde(default)]
    pub limit: Option<usize>,
}

impl Page {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE)
    }
}

/// One page of advertisements, and where the next one starts.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AdvertisementPage {
    pub advertisements: Vec<Advertisement>,
    /// Pass as `after` to continue. `None` means this was the last page.
    ///
    /// Returned rather than left for the caller to derive from the last
    /// row: a caller computing it themselves has to know the ordering,
    /// and an ordering two parties disagree about is how a page gets
    /// skipped.
    pub next_cursor: Option<AdvertisementId>,
}

impl AdvertisementFilter {
    /// Whether `ad` is one this filter asks for.
    pub fn matches(&self, ad: &Advertisement) -> bool {
        // Active unless the caller named states. See the field's doc for
        // why the default is not "everything".
        match &self.statuses {
            Some(wanted) if !wanted.contains(&ad.status) => return false,
            // An explicitly empty set asks for nothing, and gets nothing.
            // Treating it as "no constraint" would turn a client's own
            // bug into the whole book.
            None if ad.status != AdvertisementStatus::Active => return false,
            _ => {}
        }
        if let Some(merchant) = &self.merchant
            && &ad.merchant != merchant
        {
            return false;
        }
        if let Some(mint) = &self.asset_mint
            && &ad.asset_mint != mint
        {
            return false;
        }
        // A plain comparison, because `FiatCurrency` normalises at the
        // door. This used to fold case, which worked and hid the real
        // problem: the record itself could hold two spellings of one
        // currency, so the filter was compensating for an ambiguity that
        // should never have reached it.
        if let Some(fiat) = &self.fiat_currency
            && &ad.fiat_currency != fiat
        {
            return false;
        }
        if let Some(direction) = self.direction
            && ad.direction != direction
        {
            return false;
        }
        if let Some(method) = &self.payment_method
            && !ad
                .payment_methods
                .iter()
                .any(|m| m.eq_ignore_ascii_case(method))
        {
            return false;
        }
        if let Some(amount) = &self.amount {
            // Compared in base units at the advertisement's own scale. A
            // caller asking in different decimals is asking about a
            // different number, and silently rescaling would answer a
            // question they did not put.
            if amount.decimals() != ad.min_trade.decimals() {
                return false;
            }
            let units = amount.base_units();
            if units < ad.min_trade.base_units()
                || units > ad.max_trade.base_units()
                || units > ad.available_liquidity.base_units()
            {
                return false;
            }
        }
        true
    }
}

/// Applies `filter` and `page` to `all`.
///
/// Takes the whole set rather than reading the store, so the ordering and
/// the paging are testable without one — and so the caller can supply an
/// already-narrowed set once an index exists. The sort is what makes the
/// cursor stable, and it is by id: ids are unique and immutable, so the
/// order does not change when a row is published, updated or removed.
pub fn page(
    all: Vec<Advertisement>,
    filter: &AdvertisementFilter,
    page: &Page,
) -> AdvertisementPage {
    let mut matching: Vec<Advertisement> =
        all.into_iter().filter(|ad| filter.matches(ad)).collect();
    matching.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));

    let start = match &page.after {
        // Strictly after: `partition_point` gives the first index whose id
        // is greater, which is the resume point whether or not the cursor
        // row still exists. That matters — an advertisement can be deleted
        // between two pages, and a reader must not be thrown back to the
        // start because the row they last saw is gone.
        Some(cursor) => matching.partition_point(|ad| ad.id.as_str() <= cursor.as_str()),
        None => 0,
    };

    let limit = page.limit();
    let advertisements: Vec<Advertisement> = matching.into_iter().skip(start).take(limit).collect();
    // A full page does not prove there is another one, but claiming there
    // is costs a caller one empty request, while claiming there is not
    // when there is loses them rows. The cheap error is the right one.
    let next_cursor = (advertisements.len() == limit)
        .then(|| advertisements.last().map(|ad| ad.id.clone()))
        .flatten();

    AdvertisementPage {
        advertisements,
        next_cursor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::PricingModel;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_types::Timestamp;

    const USDC: &str = "2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU";
    const USDT: &str = "C4rSGhdxWhSFQuFcAxQti1JvBxriwHJoHtJjfhs5p24Y";

    fn ad(id: &str, mint: &str, fiat: &str, direction: Direction) -> Advertisement {
        let keypair = Keypair::from_seed([9u8; 32]);
        Advertisement {
            id: AdvertisementId::new(id),
            merchant: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            merchant_public_key: keypair.public_key(),
            asset_mint: MintAddress::parse(mint).unwrap(),
            direction,
            fiat_currency: FiatCurrency::parse(fiat).unwrap(),
            min_trade: Amount::new(1_000_000, 6),
            max_trade: Amount::new(100_000_000, 6),
            available_liquidity: Amount::new(100_000_000, 6),
            pricing: PricingModel::Fixed {
                price: Amount::new(129_000_000, 6),
            },
            payment_methods: vec!["M-Pesa".to_string()],
            status: AdvertisementStatus::Active,
            created_at: Timestamp::from_millis(1),
            updated_at: Timestamp::from_millis(1),
        }
    }

    fn book() -> Vec<Advertisement> {
        vec![
            ad("ad-1", USDC, "KES", Direction::Sell),
            ad("ad-2", USDT, "KES", Direction::Sell),
            ad("ad-3", USDC, "NGN", Direction::Buy),
            ad("ad-4", USDC, "KES", Direction::Buy),
        ]
    }

    fn ids(result: &AdvertisementPage) -> Vec<&str> {
        result
            .advertisements
            .iter()
            .map(|ad| ad.id.as_str())
            .collect()
    }

    #[test]
    fn an_empty_filter_is_the_whole_book() {
        let result = page(book(), &AdvertisementFilter::default(), &Page::default());
        assert_eq!(ids(&result), ["ad-1", "ad-2", "ad-3", "ad-4"]);
    }

    #[test]
    fn a_filter_narrows_by_what_a_trader_actually_chooses_by() {
        let by_mint = AdvertisementFilter {
            asset_mint: Some(MintAddress::parse(USDC).unwrap()),
            ..Default::default()
        };
        assert_eq!(
            ids(&page(book(), &by_mint, &Page::default())),
            ["ad-1", "ad-3", "ad-4"]
        );

        let usdc_for_kes_sell = AdvertisementFilter {
            asset_mint: Some(MintAddress::parse(USDC).unwrap()),
            fiat_currency: Some(FiatCurrency::parse("kes").unwrap()),
            direction: Some(Direction::Sell),
            ..Default::default()
        };
        assert_eq!(
            ids(&page(book(), &usdc_for_kes_sell, &Page::default())),
            ["ad-1"],
            "a lowercase currency code is the same code"
        );
    }

    #[test]
    fn an_amount_outside_an_advertisements_limits_excludes_it() {
        let mut small = ad("ad-small", USDC, "KES", Direction::Sell);
        small.min_trade = Amount::new(50_000_000, 6);
        let all = vec![ad("ad-1", USDC, "KES", Direction::Sell), small];

        let ten = AdvertisementFilter {
            amount: Some(Amount::new(10_000_000, 6)),
            ..Default::default()
        };
        assert_eq!(
            ids(&page(all, &ten, &Page::default())),
            ["ad-1"],
            "an offer starting at 50 is not an answer to a buyer with 10"
        );
    }

    #[test]
    fn an_amount_at_a_different_scale_is_not_silently_rescaled() {
        // 10.000000 and 10.00 are the same value written two ways, and
        // guessing which the caller meant answers a question they did not
        // ask.
        let mismatched = AdvertisementFilter {
            amount: Some(Amount::new(10_00, 2)),
            ..Default::default()
        };
        assert!(
            page(book(), &mismatched, &Page::default())
                .advertisements
                .is_empty()
        );
    }

    #[test]
    fn only_active_advertisements_are_returned_unless_asked_otherwise() {
        let mut paused = ad("ad-paused", USDC, "KES", Direction::Sell);
        paused.status = AdvertisementStatus::Vacation;
        let all = vec![ad("ad-1", USDC, "KES", Direction::Sell), paused];

        assert_eq!(
            ids(&page(
                all.clone(),
                &AdvertisementFilter::default(),
                &Page::default()
            )),
            ["ad-1"],
            "an advertisement nobody can trade against is not on offer"
        );

        let on_holiday = AdvertisementFilter {
            statuses: Some(vec![AdvertisementStatus::Vacation]),
            ..Default::default()
        };
        assert_eq!(
            ids(&page(all, &on_holiday, &Page::default())),
            ["ad-paused"]
        );
    }

    #[test]
    fn paging_returns_every_row_exactly_once() {
        let all = book();
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let result = page(
                all.clone(),
                &AdvertisementFilter::default(),
                &Page {
                    after: cursor.clone(),
                    limit: Some(2),
                },
            );
            seen.extend(
                result
                    .advertisements
                    .iter()
                    .map(|ad| ad.id.as_str().to_string()),
            );
            match result.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen, ["ad-1", "ad-2", "ad-3", "ad-4"]);
    }

    #[test]
    fn a_row_published_mid_scroll_does_not_hide_another_one() {
        // The offset bug this cursor exists to avoid. With `skip(2)`, a
        // new advertisement sorting ahead of the second page pushes a row
        // across the boundary and the reader never sees it.
        let all = book();
        let first = page(
            all.clone(),
            &AdvertisementFilter::default(),
            &Page {
                after: None,
                limit: Some(2),
            },
        );
        assert_eq!(ids(&first), ["ad-1", "ad-2"]);

        let mut grown = all;
        grown.push(ad("ad-0", USDC, "KES", Direction::Sell));

        let second = page(
            grown,
            &AdvertisementFilter::default(),
            &Page {
                after: first.next_cursor,
                limit: Some(2),
            },
        );
        assert_eq!(
            ids(&second),
            ["ad-3", "ad-4"],
            "resuming after ad-2 must not be disturbed by a row inserted before it"
        );
    }

    #[test]
    fn a_deleted_cursor_row_does_not_throw_a_reader_back_to_the_start() {
        let all = book();
        let after_two = Some(AdvertisementId::new("ad-2"));
        let mut without_two = all;
        without_two.retain(|ad| ad.id.as_str() != "ad-2");

        let result = page(
            without_two,
            &AdvertisementFilter::default(),
            &Page {
                after: after_two,
                limit: Some(2),
            },
        );
        assert_eq!(ids(&result), ["ad-3", "ad-4"]);
    }

    #[test]
    fn a_caller_cannot_ask_for_the_whole_book_by_naming_a_huge_limit() {
        let result = page(
            book(),
            &AdvertisementFilter::default(),
            &Page {
                after: None,
                limit: Some(usize::MAX),
            },
        );
        assert!(result.advertisements.len() <= MAX_PAGE);
    }

    #[test]
    fn the_last_page_says_it_is_the_last() {
        let result = page(
            book(),
            &AdvertisementFilter::default(),
            &Page {
                after: None,
                limit: Some(10),
            },
        );
        assert_eq!(result.next_cursor, None);
    }

    #[test]
    fn a_merchant_sees_their_own_book_and_nobody_elses() {
        // The question a merchant console asks. Answering it in the client
        // means serializing every advertisement on the network so a
        // browser can discard nearly all of them.
        let mut theirs = ad("ad-theirs", USDC, "KES", Direction::Sell);
        theirs.merchant = PeerId::from_bytes(vec![9; 8]);
        let mine = ad("ad-mine", USDC, "KES", Direction::Sell);
        let merchant = mine.merchant.clone();
        let all = vec![mine, theirs];

        let filter = AdvertisementFilter {
            merchant: Some(merchant),
            ..Default::default()
        };
        assert_eq!(ids(&page(all, &filter, &Page::default())), ["ad-mine"]);
    }

    #[test]
    fn a_merchants_console_gets_every_state_in_one_answer() {
        // A paused advertisement has to appear beside the live ones: this
        // is the only screen that can put it back on offer, and a screen
        // that cannot show it cannot offer that.
        let mut paused = ad("ad-paused", USDC, "KES", Direction::Sell);
        paused.status = AdvertisementStatus::Vacation;
        let mut gone = ad("ad-gone", USDC, "KES", Direction::Sell);
        gone.status = AdvertisementStatus::Deleted;
        let live = ad("ad-live", USDC, "KES", Direction::Sell);
        let merchant = live.merchant.clone();
        let all = vec![live, paused, gone];

        let filter = AdvertisementFilter {
            merchant: Some(merchant),
            statuses: Some(vec![
                AdvertisementStatus::Active,
                AdvertisementStatus::Vacation,
                AdvertisementStatus::Disabled,
                AdvertisementStatus::Deleted,
            ]),
            ..Default::default()
        };
        // Ordered by id, which is the total order the cursor resumes
        // under — not by state and not by when they were published.
        assert_eq!(
            ids(&page(all, &filter, &Page::default())),
            ["ad-gone", "ad-live", "ad-paused"]
        );
    }

    #[test]
    fn asking_for_no_statuses_returns_nothing_rather_than_everything() {
        // An empty set is a caller's bug. Reading it as "no constraint"
        // would answer it with the whole book, including the deleted rows
        // — the loudest possible response to the quietest possible
        // mistake.
        let filter = AdvertisementFilter {
            statuses: Some(Vec::new()),
            ..Default::default()
        };
        assert!(
            page(book(), &filter, &Page::default())
                .advertisements
                .is_empty()
        );
    }
}
