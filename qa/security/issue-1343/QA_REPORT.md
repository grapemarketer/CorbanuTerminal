# #1343 QA: websocket identity for header-based and brokered sign-ins

Tests for the two sign-in paths #1343 lists as not covered after #1330. The
tests change no product code. They record what each path does now.

## Setup

| Field         | Value                                                                                         |
| ------------- | --------------------------------------------------------------------------------------------- |
| Commit tested | `55ffd0277cba12bbe90dfa687a086809d14c4c10` (`main`, merge of #1318; includes #1330)            |
| OS            | Ubuntu 26.04.1 LTS, Linux 7.0.0-34-generic, x86_64 (4 CPUs, 7 GiB RAM)                        |
| Toolchain     | rustc 1.99.1 (58cf8f9c3 2026-10-13), cargo 1.99.1, from `codex-rs/rust-toolchain.toml`        |
| Native deps   | OpenSSL 3.5.5 (`libssl3t64` 3.5.5-1ubuntu3.5 runtime, `libssl-dev` 3.5.5-1ubuntu3 headers), libcap 2.75 |
| Rust deps     | `codex-rs/Cargo.lock` at the commit above                                                     |
| Network       | Loopback only (`start_websocket_server`), no provider credentials, synthetic values only      |

Run from `codex-rs/`:

```bash
cargo test -p codex-core --lib connection_identity
```

```bash
cargo test -p codex-core --test issue_1343_broker_websocket
```

Result: 6 passed in the unit-test filter (the 4 existing from #1330 and the 2
new here) and 1 passed in the broker binary. No failures.

## Case 1: header-based sign-in

Test: `client_connection_identity_tests.rs`,
`issue_1343_header_sign_in_rotation_reuses_the_prewarmed_socket`.

A provider with websockets and no `CodexAuth`/`api_auth` credential sends its
key through configured `http_headers` (`x-api-key`). Its socket is prewarmed
with `synthetic-old-key`. Turns one and two use that key and turn three uses
`synthetic-new-key`, as a credential rotation does.

| | Expected per #1343 (current, known gap) | Observed |
| --- | --- | --- |
| Resolved auth | `auth` is `None`, `api_auth` headers are empty, so the key is invisible to `ConnectionIdentity` | Same |
| Completions | `old-1`, `old-2`, `stale-3` (turn three is served on the old socket) | `old-1`, `old-2`, `stale-3` |
| Handshakes (`x-api-key`) | One, with the old key | `[Some("synthetic-old-key")]` |
| Requests per connection | `[3]` | `[3]` |

The rotated header does not replace the cached connection. This matches
#1343. The test pins the gap. Once #1343 is fixed, the expected values become
`old-1, old-2, new-3`, two handshakes (old, new) and connections `[2, 1]`, as
in #1330's bearer test.

## Case 2: credential broker sign-in

Tests: `client_connection_identity_tests.rs`,
`issue_1343_brokered_credentials_are_invisible_to_the_identity_digest`, and
`core/tests/issue_1343_broker_websocket.rs`,
`issue_1343_brokered_sign_in_never_uses_a_websocket`. The broker test is its
own binary because installing a broker is process-wide.

| | Expected per #1343 | Observed |
| --- | --- | --- |
| Identity of two different brokered credentials | Equal: Core holds only `BROKERED_KEY_PLACEHOLDER` and the brokered auth attaches no header | Equal |
| Websocket before a broker is installed | Enabled for the provider | `responses_websocket_enabled() == true` |
| Websocket after a broker is installed (old and rotated credential) | Disabled, so no cached socket can carry one brokered sign-in into another (PF-27-S05) | `false` for both |

The digest cannot see brokered credentials, as #1343 says. A removed account
still cannot be used over a stale socket because no websocket is used under the
broker.

## Not covered

- `env_http_headers` uses the same header map as `http_headers` and is not
  tested separately.
- The tests use a test broker, not the contained broker process.
- These were run with `cargo test`, not nextest or Bazel.
