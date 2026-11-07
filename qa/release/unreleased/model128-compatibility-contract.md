# Retained model IDs: local compatibility contract

Prepared October 23, 2026 for current main, following the [PR #141 maintainer
request][retarget]. This regression-only change covers local compatibility for
[issue #128][issue] and the proposed [PR #140 specification][spec]. The adapted
tests have since run on this main-based tree (see [Test runs](#test-runs)). This
record does not close all of issue #128 or establish that a provider currently
serves a retained model.

Change class: **Routine** tests and QA documentation under `AGENTS.md` for the
PR #141 part. The 2026-10-28 dispositions below (PR #1486) are a **Bounded
fix**: they edit one catalogue record's `reason` and `description` text, and
narrow one compatibility claim to match upstream reality, as the maintainers
directed on #128 on 2026-10-21 ([decision][direction]). Product basis: [Shipping
MVP — LIVE][product], **Multi-provider inference**: “OpenAI, Anthropic/Claude
Plan, Kimi, Z.AI, DeepSeek, OpenRouter, Ambient, Meta, Baseten, Vercel, Bedrock,
Ollama, LM Studio, Corbanu Plan, and custom providers.” No routing policy,
credential, selection state or output cap changes; no product-initiative plan
or sprint is introduced.

## Revision and policy boundary

The baseline is main `55ffd0277cba12bbe90dfa687a086809d14c4c10`.
The catalogue retains these exact IDs and output metadata:

| Provider | Retained ID | Catalogue `max_output_tokens` |
| --- | --- | --- |
| OpenRouter | `x-ai/grok-4.6` | 128,000 |
| OpenRouter | `openrouter/owl-alpha` | 262,144 |
| OpenRouter | `x-ai/grok-4.5` | 128,000 |
| OpenRouter | `deepseek/deepseek-v4-pro` | 384,000 |
| OpenRouter | `deepseek/deepseek-v4-flash-0731` | 384,000 |
| OpenAI | `gpt-5.5` | absent |

At this main pin, all six records are hidden from the picker and disabled for
automatic allocation. Five have explicit output metadata; `gpt-5.5` has none.
The regressions retain explicit-ID resolution without changing these catalogue
records. These are source facts and intended assertions, not new runtime proof,
upstream service availability or a historical retirement date. The original
release-based tests and QA history remain bound to commit
`0a636facef219106f6c13dd04fad86b95ba5574d`.

## Dispositions (2026-10-28)

Each retained ID now has an explicit disposition, following the
[10-21 direction][direction]. `catalogue_issue_128_retained_ids_have_explicit_dispositions`
(models-manager) pins this table; change both together. All six stay hidden and
disabled for automatic allocation.

| ID | Disposition | Evidence (read 2026-10-28 ~15:50 UTC) |
| --- | --- | --- |
| `x-ai/grok-4.6` | Served; cap 128,000 kept | OpenRouter [endpoints API][or-g46]: xAI and Amazon Bedrock endpoints, 450,000 listed max. 128,000 is the deliberate conservative cap from #189 (the Anthropic-wire route always asks for the full cap, so 450,000 would break conversations past ~50K tokens). |
| `x-ai/grok-4.5` | Served; cap 128,000 kept | OpenRouter [endpoints API][or-g45]: four xAI endpoints, 450,000 listed. Same reasoning as Grok 4.6. |
| `deepseek/deepseek-v4-pro` | Served; cap 384,000 kept | OpenRouter [endpoints API][or-dsp]: StreamLake and DigitalOcean list 384,000; others list 16,384 to 943,718. Matches the direct DeepSeek and Vercel rows (#189). |
| `deepseek/deepseek-v4-flash-0731` | Served; cap 384,000 kept | OpenRouter [endpoints API][or-dsf]: DeepInfra and BaseTen list 384,000; others 131,072 to 943,718. Matches the direct DeepSeek flash rows (#189). |
| `openrouter/owl-alpha` | **Retired upstream; row kept with a retirement note** | OpenRouter's [endpoints API][or-owl] returns the model record with `"endpoints": []`, and the ID is absent from the [models list][or-list]. #189 already recorded it as retired on 2026-10-06, so this is two signals three weeks apart, not one missing feed entry. |
| `gpt-5.5` | Served; cap deliberately omitted | OpenAI's [model page][oai-55] lists GPT-5.5 with "128,000 max output tokens", and it is still on the [Standard price page][oai-price] ($5 / $0.50 / $30, matching the catalogue). The `openai` provider uses the Responses wire, which never sends an output cap (see the core test below), so OpenAI's own limit applies. The row is also the template for the Amazon Bedrock GPT rows (`model-provider/src/amazon_bedrock/catalog.rs`), also on the Responses wire, so a cap here would be copied onto Bedrock routes whose limits were not verified. The other GPT-5.x rows also state none; the GPT-6 rows state 128,000, which is metadata only on this wire. That split is left as it is. |

**Retired ID behaviour (`openrouter/owl-alpha`).** A saved `model =
"openrouter/owl-alpha"` still resolves to its own bundled row (its own 262,144
cap, no fallback metadata, no substitution). Requests go to OpenRouter, which
has no endpoint for it, so they are expected to fail with OpenRouter's error
(inferred from the empty endpoint list; no live request was sent). There is no
automatic migration: OpenRouter names no successor. The row's reason and
description now say it is retired and point to `/model`, and the OpenRouter
docs and example config no longer list it. The compatibility claim for this ID
is narrowed to "a saved ID resolves locally"; it is not claimed to work.

## What the tests exercise

The `catalogue_issue_128` model-manager tests extend the existing generic hidden
model/cache regression. They resolve each saved explicit-ID input through the
real model-manager methods and verify exact identity, provider, hidden/non-default
picker state, preserved disabled orchestration, the bundled row's output metadata,
and absence of fallback metadata in four states:

1. Discovery is unavailable and no cache exists; no endpoint fetch occurs.
2. Live discovery tries to re-list hidden models and replace their provider and
   orchestration policy with an eligible route. A separate discovered model has
   a positive output limit, which must not leak into a retained ID.
3. A new manager reads the fresh persisted cache; no extra endpoint fetch occurs.
4. The persisted cache is aged past its TTL, then a new manager refetches an empty
   discovery result. Old unrelated discovery records disappear, while all six
   saved IDs still resolve from the bundle.

The core `catalogue_issue_128_saved_id_wire_requests_omit_output_limits` test
passes each exact saved ID through the static model manager, then calls the real
OpenRouter Chat Completions or OpenAI Responses request builder with a user
message. It serializes the returned request and verifies its exact model ID.
For each model it first uses that row's current output metadata, then repeats
construction with metadata absent, zero, and 8,192. It asserts that the entire
serialized request remains equal, and none of `max_tokens`,
`max_completion_tokens`, or `max_output_tokens` is present—not even as JSON null.
The current builders and serialization types still omit these wire keys; adding
catalogue caps did not add fields to these two request formats. The real provider
constructors and wire API variants are used.

The source-level contract on these paths remains wire-limit omission. A catalogue
value alone is not an enforced output budget in these request types. This record
makes no claim about a provider's current default or maximum, nor a new observed
runtime result.

## Reproduction and qualification

Use the pinned repository toolchain and installed `just`/`cargo-nextest`. From
the repository root, after formatting the final tree:

```sh
just fmt
just test --locked -p codex-models-manager -E 'test(catalogue_issue_128)'
just test --locked -p codex-core --lib -E 'test(catalogue_issue_128)'
just test --locked -p codex-models-manager
just test --locked -p codex-core
```

Nextest fails when a filter matches zero tests. The filter matches three manager
tests: PR #141's two regressions, which cover 24 six-ID/state assertions, and
PR #1486's `catalogue_issue_128_retained_ids_have_explicit_dispositions`, which
makes one whole-row assertion per ID. The core matrix builds and serializes 24
requests (including `gpt-5.5`'s repeated absent-metadata case). Record the tested
PR head and actual command outcomes in the PR, including tool or baseline
failures. A passing focused run does not imply a passing workspace.

### Test runs

- PR #141's replayed head: all 78 `codex-models-manager` tests passed, including
  its two manager regressions; the core wire test was not run there.
- PR #1486's head: `cargo test --locked -p codex-models-manager --lib
  catalogue_issue_128` passed 3/3.
- main `5ed2aca27249b3657ee52e14b10b844c5cfe4c8d` (2026-10-28, Linux debug
  build, `RUST_MIN_STACK=8388608` and a disposable `CODEX_HOME`): `cargo test
  --locked -p codex-models-manager --lib catalogue_issue_128` passed 3/3, and
  `cargo test --locked -p codex-core --lib catalogue_issue_128` passed 1/1
  (`catalogue_issue_128_saved_id_wire_requests_omit_output_limits`).

These are focused runs, not a full-suite or release qualification. Their scope
is local resolution, cache transitions, and builder/serializer behavior. They do
**not** exercise reading saved profile/session files, a live TUI, credentials, HTTP transport or endpoint acceptance, current provider serving
status, token-capacity boundaries, endpoint fallback, billing, or a release.
No live inference calls are required. Those limits are material: provider
retirement/migration decisions and the broader #129 output-budget policy need
separate evidence and authorization.

[issue]: https://github.com/CorbanuCore/CorbanuTerminal/issues/128
[spec]: https://github.com/CorbanuCore/CorbanuTerminal/blob/28490d22b53a99485eabbfe8f0ba125b7845e71d/MODEL-CATALOGUE-METADATA-REMEDIATION-SPEC.md
[product]: https://github.com/CorbanuCore/CorbanuTerminal/blob/55ffd0277cba12bbe90dfa687a086809d14c4c10/docs/corbanu-product-spec.md#shipping-mvp--live
[retarget]: https://github.com/CorbanuCore/CorbanuTerminal/pull/141#issuecomment-6312414499
[direction]: https://github.com/CorbanuCore/CorbanuTerminal/issues/128#issuecomment-6322464397
[or-g46]: https://openrouter.ai/api/v1/models/x-ai/grok-4.6/endpoints
[or-g45]: https://openrouter.ai/api/v1/models/x-ai/grok-4.5/endpoints
[or-dsp]: https://openrouter.ai/api/v1/models/deepseek/deepseek-v4-pro/endpoints
[or-dsf]: https://openrouter.ai/api/v1/models/deepseek/deepseek-v4-flash-0731/endpoints
[or-owl]: https://openrouter.ai/api/v1/models/openrouter/owl-alpha/endpoints
[or-list]: https://openrouter.ai/api/v1/models
[oai-55]: https://developers.openai.com/api/docs/models/gpt-5.5
[oai-price]: https://developers.openai.com/api/docs/pricing
