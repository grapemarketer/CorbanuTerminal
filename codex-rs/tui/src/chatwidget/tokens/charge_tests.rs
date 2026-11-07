//! #368 (Travis, 2026-10-14): `/cost` prices an OpenRouter request from the
//! charge OpenRouter reported with its response (`usage.cost`), shows the
//! catalogue rate only as a labelled estimate when no charge was reported, and
//! "no price" when there is neither. Each page says which basis it used.

use super::super::InspectorPage;
use super::super::inspection_pages;
use chrono::NaiveDate;
use codex_protocol::ThreadId;
use codex_state::accounting::Attempt;
use codex_state::accounting::BucketQuote;
use codex_state::accounting::DayTotals;
use codex_state::accounting::Decimal;
use codex_state::accounting::Dialect;
use codex_state::accounting::Inspection;
use codex_state::accounting::InspectionDay;
use codex_state::accounting::ObservationQuote;
use codex_state::accounting::OtherConversations;
use codex_state::accounting::RetentionCoverage;
use codex_state::accounting::Usage;
use pretty_assertions::assert_eq;
use uuid::Uuid;

const DAY_MS: i64 = 86_400_000;

fn utc_day() -> i64 {
    NaiveDate::from_ymd_opt(2026, 10, 14)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|time| time.and_utc().timestamp() / 86_400)
        .unwrap()
}

fn owner() -> ThreadId {
    ThreadId::from_string(&Uuid::from_u128(1).to_string()).unwrap()
}

fn decimal(text: &str) -> Decimal {
    text.to_owned().try_into().unwrap()
}

/// A non-streaming OpenRouter chat completion as OpenRouter sends it; the
/// streaming path ends with the same `usage` object.
fn openrouter_body(usage_extra: serde_json::Value) -> Vec<u8> {
    let mut usage = serde_json::json!({
        "prompt_tokens": 100,
        "prompt_tokens_details": {"cached_tokens": 20},
        "completion_tokens": 10,
        "total_tokens": 110
    });
    for (key, value) in usage_extra.as_object().unwrap() {
        usage[key] = value.clone();
    }
    serde_json::to_vec(&serde_json::json!({
        "id": "gen-368",
        "provider": "SomeUpstream",
        "model": "upstream/model",
        "object": "chat.completion",
        "choices": [{"index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": "ok"}}],
        "usage": usage
    }))
    .unwrap()
}

/// The charge the client records from an OpenRouter response, read by the
/// same parser the accounting path uses.
fn recorded_charge(body: &[u8]) -> Option<Decimal> {
    codex_api::chat_body_usage(body)
        .unwrap()
        .unwrap()
        .billed_usd
        .map(|text| text.try_into().unwrap())
}

/// An OpenRouter attempt for the response `body`: 80 noncached, 20 cache
/// read, no cache writes and 10 output tokens. With `catalogue`, the price
/// bound at dispatch makes that $0.00071 ($5/M, $0.50/M, $30/M); without it
/// no published price covers the model.
fn attempt(id: u128, model: &str, catalogue: bool, body: &[u8]) -> ObservationQuote {
    let priced = |amount: &str| BucketQuote::Priced(decimal(amount));
    let (buckets, known) = if catalogue {
        (
            [
                priced("0.0004"),
                priced("0.00001"),
                priced("0"),
                priced("0.0003"),
            ],
            decimal("0.00071"),
        )
    } else {
        (
            [
                BucketQuote::MissingRate,
                BucketQuote::MissingRate,
                priced("0"),
                BucketQuote::MissingRate,
            ],
            Decimal::default(),
        )
    };
    ObservationQuote {
        attempt: Attempt {
            attempt_id: Uuid::from_u128(id),
            request_id: Uuid::from_u128(1000 + id),
            thread_id: owner(),
            turn: "synthetic".into(),
            retry_of: None,
            provider: "openrouter".into(),
            model: model.into(),
            scope: Uuid::from_u128(5),
            dialect: Dialect::Inclusive,
            dispatched_at_ms: (utc_day() * DAY_MS + DAY_MS / 2 + i64::try_from(id).unwrap())
                .try_into()
                .unwrap(),
            account: None,
        },
        observations: vec![],
        snapshot: None,
        usage: Usage {
            input: Some(100),
            noncached: Some(80),
            read: Some(20),
            write: Some(0),
            output: Some(10),
            total: Some(110),
            billed_usd: recorded_charge(body),
            ..Usage::default()
        },
        buckets,
        known_subtotal: known,
        all_buckets_priced: catalogue.then_some(known),
        subtotal_display: known.display(),
        known_equivalent: Decimal::default(),
        all_buckets_equivalent: None,
        plan_burn_millis: None,
        plan_burn_milli_tokens: None,
        pricing_rules: 1,
    }
}

fn by_request(
    quotes: Vec<ObservationQuote>,
) -> std::collections::BTreeMap<Uuid, Vec<ObservationQuote>> {
    let mut requests = std::collections::BTreeMap::<Uuid, Vec<ObservationQuote>>::new();
    for quote in quotes {
        requests
            .entry(quote.attempt.request_id)
            .or_default()
            .push(quote);
    }
    requests
}

/// The open conversation's day, as the store returns it: totals summed from
/// the recorded quotes.
fn day(quotes: Vec<ObservationQuote>) -> InspectionDay {
    let requests = by_request(quotes);
    let totals = DayTotals::from_quotes(requests.values().flatten()).unwrap();
    let read_at_ms = utc_day() * DAY_MS + DAY_MS * 3 / 4;
    InspectionDay::Ready(Inspection {
        owner: owner(),
        utc_day: utc_day(),
        read_at_ms,
        coverage: RetentionCoverage {
            completed_as_of_ms: read_at_ms,
            detail_expired_through_ms: None,
            aggregate_day_floor: 0,
            oldest_recorded_day: Some(utc_day()),
        },
        own_totals: totals.clone(),
        totals,
        descendant_totals: DayTotals::default(),
        unknown_parent_totals: DayTotals::default(),
        unknown_parent_unavailable_threads: 0,
        unknown_parent_requests: Default::default(),
        requests,
        other_conversations: Some(OtherConversations::default()),
        empty_day: None,
        recording_gap: false,
    })
}

fn first_screen(page: &InspectorPage) -> Vec<String> {
    page.text
        .iter()
        .take_while(|line| line.as_str() != "—— Details ——")
        .cloned()
        .collect()
}

/// The request page whose first-screen link names `model`.
fn request_page<'a>(pages: &'a [InspectorPage], model: &str) -> &'a InspectorPage {
    let (_, index) = pages[0]
        .links
        .iter()
        .find(|(label, _)| label.starts_with("Request ") && label.contains(model))
        .unwrap_or_else(|| panic!("no request link for {model}: {:?}", pages[0].links));
    &pages[*index]
}

fn has(page: &InspectorPage, line: &str) -> bool {
    page.text.iter().any(|text| text == line)
}

/// Four OpenRouter requests: one that reported a charge and has a catalogue
/// price, one with a catalogue price and no charge, one with neither, and one
/// with a charge but no catalogue price.
fn openrouter_day() -> Vec<InspectorPage> {
    let charged = openrouter_body(serde_json::json!({"cost": 0.000123, "is_byok": false}));
    let silent = openrouter_body(serde_json::json!({}));
    // Bring-your-own-key: the figure is only OpenRouter's fee, so it is not
    // the charge for the request.
    let byok = openrouter_body(serde_json::json!({"cost": 0.000005, "is_byok": true}));
    let unlisted_charged = openrouter_body(serde_json::json!({"cost": 5e-5}));
    inspection_pages(Ok(day(vec![
        attempt(
            /*id*/ 1,
            "x-ai/grok-4.7",
            /*catalogue*/ true,
            &charged,
        ),
        attempt(
            /*id*/ 2,
            "minimax/minimax-m3",
            /*catalogue*/ true,
            &byok,
        ),
        attempt(
            /*id*/ 3,
            "some/unlisted-model",
            /*catalogue*/ false,
            &silent,
        ),
        attempt(
            /*id*/ 4,
            "moonshotai/kimi-k3",
            /*catalogue*/ false,
            &unlisted_charged,
        ),
    ])))
}

#[test]
fn mocked_openrouter_responses_record_the_charge_only_when_stated() {
    assert_eq!(
        recorded_charge(&openrouter_body(serde_json::json!({"cost": 0.000123}))),
        Some(decimal("0.000123"))
    );
    assert_eq!(
        recorded_charge(&openrouter_body(serde_json::json!({"cost": 5e-5}))),
        Some(decimal("0.00005"))
    );
    assert_eq!(
        recorded_charge(&openrouter_body(serde_json::json!({}))),
        None
    );
    assert_eq!(
        recorded_charge(&openrouter_body(
            serde_json::json!({"cost": 0.000005, "is_byok": true})
        )),
        None
    );
}

#[test]
fn openrouter_request_with_a_reported_charge_is_priced_from_it() {
    let pages = openrouter_day();
    let page = request_page(&pages, "OpenRouter Grok 4.7");
    // The charge, not the $0.000710 catalogue estimate, and labelled as such.
    assert!(
        has(page, "Cost: $0.000123, charged by OpenRouter"),
        "{:#?}",
        page.text
    );
    assert!(
        has(
            page,
            "Price basis: the charge OpenRouter reported with the response (usage.cost)"
        ),
        "{:#?}",
        page.text
    );
    assert!(
        !page.text.iter().any(|line| line.contains("Estimated cost")),
        "{:#?}",
        page.text
    );
    // The provider's charge is the cost here, so it is not repeated as a
    // second figure on the request's header.
    assert!(
        !page
            .text
            .iter()
            .any(|line| line.starts_with("Billed by provider:")),
        "{:#?}",
        page.text
    );
    // The attempt page keeps the catalogue breakdown for audit, marked unused.
    let attempt = &pages[page.links[0].1];
    assert!(
        has(
            attempt,
            "Published-price estimate (not used — OpenRouter reported its charge): $0.000710"
        ),
        "{:#?}",
        attempt.text
    );
    assert!(has(attempt, "Known subtotal exact USD: 0.000123"));
}

#[test]
fn openrouter_request_without_a_charge_falls_back_to_a_labelled_estimate() {
    let pages = openrouter_day();
    let page = request_page(&pages, "OpenRouter MiniMax M3");
    assert!(has(page, "Estimated cost: $0.000710"), "{:#?}", page.text);
    assert!(
        has(
            page,
            "Price basis: estimate from published prices — OpenRouter reported no charge for this response"
        ),
        "{:#?}",
        page.text
    );
}

#[test]
fn openrouter_request_with_neither_has_no_price() {
    let pages = openrouter_day();
    let page = request_page(&pages, "some/unlisted-model");
    assert!(
        has(page, "Estimated cost: no price available"),
        "{:#?}",
        page.text
    );
    assert!(
        has(
            page,
            "Price basis: none — OpenRouter reported no charge, and no published price covers this response"
        ),
        "{:#?}",
        page.text
    );
}

#[test]
fn openrouter_charge_prices_a_model_the_catalogue_does_not_list() {
    let pages = openrouter_day();
    let page = request_page(&pages, "OpenRouter Kimi K3");
    assert!(
        has(page, "Cost: $0.000050, charged by OpenRouter"),
        "{:#?}",
        page.text
    );
    // Not named among the attempts with no price.
    assert!(
        !pages[0]
            .text
            .iter()
            .any(|line| line.starts_with("Attempts with no price") && line.contains("kimi")),
        "{:#?}",
        pages[0].text
    );
}

#[test]
fn cost_overview_states_each_routes_basis_and_sums_charges() {
    let pages = openrouter_day();
    let screen = first_screen(&pages[0]);
    for line in [
        "• OpenRouter · OpenRouter Kimi K3 — Pay per use. 1 request, 110 tokens. Cost: $0.000050, charged by OpenRouter.",
        "• OpenRouter · OpenRouter MiniMax M3 — Pay per use. 1 request, 110 tokens. Estimated cost: $0.000710.",
        "• OpenRouter · some/unlisted-model — Pay per use. 1 request, 110 tokens. Estimated cost: no price available.",
        "• OpenRouter · OpenRouter Grok 4.7 — Pay per use. 1 request, 110 tokens. Cost: $0.000123, charged by OpenRouter.",
        "Costs marked \"charged by OpenRouter\" are the charges OpenRouter reported with those responses; other costs are estimates from published prices; OpenRouter stated no charge for this work — check OpenRouter's bill.",
    ] {
        assert!(screen.contains(&line.to_string()), "{line}\n{screen:#?}");
    }
    // The day's known cost is the two charges plus the one estimate:
    // 0.000123 + 0.00005 + 0.00071, never the charged requests' catalogue
    // estimates; the request with no price keeps the total a lower bound.
    assert!(
        has(
            &pages[0],
            "Known token cost: $0.000883 + unknown costs ($0.000173 charged by OpenRouter + $0.000710 estimated from published prices)"
        ),
        "{:#?}",
        pages[0].text
    );
    assert!(has(&pages[0], "Known subtotal exact USD: 0.000883"));
    // The provider page totals the same way.
    let (_, provider) = pages[0]
        .links
        .iter()
        .find(|(label, _)| label.starts_with("OpenRouter · OpenRouter Grok 4.7"))
        .unwrap();
    assert!(
        has(&pages[*provider], "Known cost exact USD: 0.000123"),
        "{:#?}",
        pages[*provider].text
    );
}

#[test]
fn every_openrouter_page_names_its_price_basis() {
    let pages = openrouter_day();
    for page in pages
        .iter()
        .filter(|page| page.title == "Request" || page.title.starts_with("Attempt"))
    {
        assert_eq!(
            page.text
                .iter()
                .filter(|line| line.starts_with("Price basis:"))
                .count(),
            1,
            "{}: {:#?}",
            page.title,
            page.text
        );
    }
}

/// A pay-per-use attempt on another provider whose output count was not
/// reported: $0.00041 of its published-price estimate is known.
fn partly_priced(id: u128) -> ObservationQuote {
    let mut quote = attempt(
        id,
        "deepseek-chat",
        /*catalogue*/ true,
        &openrouter_body(serde_json::json!({})),
    );
    quote.attempt.provider = "deepseek".into();
    quote.usage.billed_usd = None;
    quote.usage.output = None;
    quote.buckets[3] = BucketQuote::MissingUsage;
    quote.known_subtotal = decimal("0.00041");
    quote.all_buckets_priced = None;
    quote.subtotal_display = quote.known_subtotal.display();
    quote
}

/// Review of #784: a partly priced attempt elsewhere is an estimate, not part
/// of OpenRouter's charge, and the two parts add up to the known cost.
#[test]
fn charge_and_estimate_parts_add_up_with_a_partly_priced_attempt() {
    let charged = openrouter_body(serde_json::json!({"cost": 0.000123}));
    let pages = inspection_pages(Ok(day(vec![
        attempt(
            /*id*/ 1,
            "x-ai/grok-4.7",
            /*catalogue*/ true,
            &charged,
        ),
        partly_priced(/*id*/ 2),
    ])));
    assert!(
        has(
            &pages[0],
            "Known token cost: $0.000533 + unknown costs ($0.000123 charged by OpenRouter + $0.000410 estimated from published prices)"
        ),
        "{:#?}",
        pages[0].text
    );
    // The other provider's page keeps its own estimate wording.
    let (_, deepseek) = pages[0]
        .links
        .iter()
        .find(|(label, _)| label.starts_with("DeepSeek"))
        .unwrap();
    assert!(
        pages[*deepseek]
            .text
            .iter()
            .any(|line| line.starts_with("Estimated cost: at least $0.000410")),
        "{:#?}",
        pages[*deepseek].text
    );
}

/// A free model's reported charge of $0 is a cost of $0, not a missing price.
#[test]
fn a_zero_charge_is_a_stated_cost() {
    let free = openrouter_body(serde_json::json!({"cost": 0}));
    let pages = inspection_pages(Ok(day(vec![attempt(
        /*id*/ 1,
        "tencent/hy3:free",
        /*catalogue*/ false,
        &free,
    )])));
    let page = request_page(&pages, "Hy3");
    assert!(
        has(page, "Cost: $0.000000, charged by OpenRouter"),
        "{:#?}",
        page.text
    );
    assert!(!pages[0].text.iter().any(|line| line.contains("no price")));
}

/// A refused attempt states no charge and has no price basis to name.
#[test]
fn a_refused_openrouter_attempt_has_no_price_basis() {
    let mut refused = attempt(
        /*id*/ 1,
        "x-ai/grok-4.7",
        /*catalogue*/ true,
        &openrouter_body(serde_json::json!({"cost": 0.5})),
    );
    refused.usage = Usage {
        refused_status: Some(402),
        ..Usage::default()
    };
    refused.buckets = [BucketQuote::Priced(Decimal::default()); 4];
    refused.known_subtotal = Decimal::default();
    refused.all_buckets_priced = Some(Decimal::default());
    let pages = inspection_pages(Ok(day(vec![refused])));
    let page = request_page(&pages, "Grok 4.7");
    assert!(
        !page
            .text
            .iter()
            .any(|line| line.starts_with("Price basis:") || line.contains("charged by")),
        "{:#?}",
        page.text
    );
}

/// The day's other conversations and a range read the same charges.
#[test]
fn other_conversations_and_ranges_use_the_charge() {
    let charged = openrouter_body(serde_json::json!({"cost": 0.000123}));
    let InspectionDay::Ready(mut view) = day(Vec::new()) else {
        unreachable!()
    };
    let mut other = attempt(
        /*id*/ 9,
        "x-ai/grok-4.7",
        /*catalogue*/ true,
        &charged,
    );
    other.attempt.thread_id = ThreadId::from_string(&Uuid::from_u128(2).to_string()).unwrap();
    view.other_conversations = Some(OtherConversations {
        conversations: 1,
        requests: by_request(vec![other]),
        ..OtherConversations::default()
    });
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    assert!(
        has(
            &pages[0],
            "Other conversations' cost: $0.000123, charged by OpenRouter."
        ),
        "{:#?}",
        pages[0].text
    );

    let InspectionDay::Ready(view) = day(vec![
        attempt(
            /*id*/ 1,
            "x-ai/grok-4.7",
            /*catalogue*/ true,
            &charged,
        ),
        attempt(
            /*id*/ 2,
            "minimax/minimax-m3",
            /*catalogue*/ true,
            &openrouter_body(serde_json::json!({})),
        ),
    ]) else {
        unreachable!()
    };
    let start = utc_day() * DAY_MS;
    let range = InspectionDay::Range {
        requested: codex_state::accounting::InspectionRange {
            start_ms: start,
            end_ms: start + DAY_MS,
            grouping: codex_state::accounting::InspectionGrouping::Day,
        },
        aggregate_day_floor: Some(0),
        completed_as_of_ms: None,
        read_at_ms: start + 2 * DAY_MS,
        buckets: vec![codex_state::accounting::InspectionBucket {
            start_ms: start,
            end_ms: start + DAY_MS,
            effective: Some((start, start + DAY_MS)),
            partial: false,
            days: vec![InspectionDay::Ready(view)],
        }],
    };
    let pages = inspection_pages(Ok(range));
    let line = "Token cost for recorded attempts: $0.000833 ($0.000123 charged by OpenRouter + $0.000710 estimated from published prices)";
    // The range overview, and the bucket's own pages.
    assert!(has(&pages[0], line), "{:#?}", pages[0].text);
    assert!(
        pages[1..].iter().any(|page| has(page, line)),
        "{:#?}",
        pages.iter().map(|page| &page.text).collect::<Vec<_>>()
    );
}

/// Review of #784, round 2: a refused attempt beside a charged one adds no
/// "$0 estimated" part.
#[test]
fn a_refused_attempt_beside_a_charge_adds_no_estimate_part() {
    let charged = openrouter_body(serde_json::json!({"cost": 0.000123}));
    let mut refused = attempt(
        /*id*/ 2,
        "x-ai/grok-4.7",
        /*catalogue*/ true,
        &charged,
    );
    refused.usage = Usage {
        refused_status: Some(401),
        ..Usage::default()
    };
    refused.buckets = [BucketQuote::Priced(Decimal::default()); 4];
    refused.known_subtotal = Decimal::default();
    refused.all_buckets_priced = Some(Decimal::default());
    let pages = inspection_pages(Ok(day(vec![
        attempt(
            /*id*/ 1,
            "x-ai/grok-4.7",
            /*catalogue*/ true,
            &charged,
        ),
        refused,
    ])));
    let screen = first_screen(&pages[0]);
    let line = screen
        .iter()
        .find(|line| line.starts_with("• OpenRouter · OpenRouter Grok 4.7"))
        .unwrap();
    assert!(
        line.contains(" Cost: $0.000123, charged by OpenRouter.") && !line.contains("estimated"),
        "{line}"
    );
}

/// An OpenRouter attempt with no charge and only part of its published price
/// known is a lower-bound estimate, and says OpenRouter reported no charge.
#[test]
fn openrouter_attempt_priced_in_part_is_a_lower_bound_estimate() {
    let silent = openrouter_body(serde_json::json!({}));
    let mut partial = attempt(
        /*id*/ 1,
        "x-ai/grok-4.7",
        /*catalogue*/ true,
        &silent,
    );
    partial.usage.output = None;
    partial.buckets[3] = BucketQuote::MissingUsage;
    partial.known_subtotal = decimal("0.00041");
    partial.all_buckets_priced = None;
    let mut charged = attempt(
        /*id*/ 2,
        "x-ai/grok-4.7",
        /*catalogue*/ true,
        &openrouter_body(serde_json::json!({"cost": 0.000123})),
    );
    charged.attempt.request_id = partial.attempt.request_id;
    charged.attempt.retry_of = Some(partial.attempt.attempt_id);
    let pages = inspection_pages(Ok(day(vec![partial, charged])));
    let page = request_page(&pages, "Grok 4.7");
    assert!(
        has(
            page,
            "Price basis: 1 attempt priced from the charge OpenRouter reported; 1 attempt estimated in part from published prices (a lower bound) — OpenRouter reported no charge for 1 attempt"
        ),
        "{:#?}",
        page.text
    );
    let partial_page = &pages[page.links[0].1];
    assert!(
        has(
            partial_page,
            "Price basis: partial estimate from published prices, a lower bound — OpenRouter reported no charge for this response"
        ),
        "{:#?}",
        partial_page.text
    );
}

/// `quote` as an attempt on `provider`, charged `charge` if any.
fn on(provider: &str, charge: Option<&str>, mut quote: ObservationQuote) -> ObservationQuote {
    quote.attempt.provider = provider.into();
    quote.usage.billed_usd = charge.map(decimal);
    quote
}

/// Travis, 2026-10-14: the Vercel AI Gateway and the Corbanu API report what
/// they charged with each response, so their requests are priced from that
/// charge by the same rule as OpenRouter's, with the catalogue rate as a
/// labelled estimate when no charge was reported.
#[test]
fn vercel_and_corbanu_api_requests_are_priced_from_their_charge() {
    for (provider, name, field) in [
        ("vercel", "Vercel", "gatewayCost"),
        (
            "pfterminal-plan",
            "Corbanu API",
            "usage.corbanu_charge_usd or usage.cost",
        ),
        (
            "pfterminal-plan-anthropic",
            "Corbanu API",
            "usage.corbanu_charge_usd",
        ),
    ] {
        let silent = openrouter_body(serde_json::json!({}));
        let pages = inspection_pages(Ok(day(vec![
            on(
                provider,
                Some("0.000123"),
                attempt(
                    /*id*/ 1,
                    "charged/model",
                    /*catalogue*/ true,
                    &silent,
                ),
            ),
            on(
                provider,
                /*charge*/ None,
                attempt(
                    /*id*/ 2,
                    "silent/model",
                    /*catalogue*/ true,
                    &silent,
                ),
            ),
        ])));
        let charged = request_page(&pages, "charged/model");
        assert!(
            has(charged, &format!("Cost: $0.000123, charged by {name}")),
            "{provider}: {:#?}",
            charged.text
        );
        assert!(
            has(
                charged,
                &format!("Price basis: the charge {name} reported with the response ({field})")
            ),
            "{provider}: {:#?}",
            charged.text
        );
        // The charge is the cost, so it is not repeated as a second figure.
        assert!(
            !charged
                .text
                .iter()
                .any(|line| line.starts_with("Billed by provider:")),
            "{provider}: {:#?}",
            charged.text
        );
        let attempt_page = &pages[charged.links[0].1];
        assert!(
            has(
                attempt_page,
                &format!(
                    "Published-price estimate (not used — {name} reported its charge): $0.000710"
                )
            ),
            "{provider}: {:#?}",
            attempt_page.text
        );
        assert!(has(attempt_page, "Known subtotal exact USD: 0.000123"));
        let estimated = request_page(&pages, "silent/model");
        assert!(
            has(estimated, "Estimated cost: $0.000710"),
            "{provider}: {:#?}",
            estimated.text
        );
        assert!(
            has(
                estimated,
                &format!(
                    "Price basis: estimate from published prices — {name} reported no charge for this response"
                )
            ),
            "{provider}: {:#?}",
            estimated.text
        );
        // The day is the charge plus the estimate, never the charged
        // request's catalogue estimate.
        assert!(
            has(&pages[0], "Known subtotal exact USD: 0.000833"),
            "{provider}: {:#?}",
            pages[0].text
        );
    }
}

/// Charges from several routes on one screen name every route they came from.
#[test]
fn charges_from_several_routes_name_each_provider() {
    let silent = openrouter_body(serde_json::json!({}));
    let pages = inspection_pages(Ok(day(vec![
        on(
            "openrouter",
            Some("0.0001"),
            attempt(/*id*/ 1, "a/model", /*catalogue*/ true, &silent),
        ),
        on(
            "vercel",
            Some("0.0002"),
            attempt(/*id*/ 2, "b/model", /*catalogue*/ true, &silent),
        ),
    ])));
    let screen = first_screen(&pages[0]);
    for line in [
        "• OpenRouter · a/model — Pay per use. 1 request, 110 tokens. Cost: $0.000100, charged by OpenRouter.",
        "• Vercel · b/model — Pay per use. 1 request, 110 tokens. Cost: $0.000200, charged by Vercel.",
        "Costs marked \"charged by\" a provider (OpenRouter and Vercel) are the charges that provider reported with those responses.",
    ] {
        assert!(screen.contains(&line.to_string()), "{line}\n{screen:#?}");
    }
    assert!(
        has(
            &pages[0],
            "Token cost for recorded attempts: $0.000300, charged by OpenRouter and Vercel"
        ),
        "{:#?}",
        pages[0].text
    );
}
