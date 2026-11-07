# Task Node evidence input and handoff review

Task: `task_fe1cea017341b7ade311b418e19fb308`. Reviewed 2026-10-07.

CorbanuTerminal base: `4f09d7af996b9b5fb68ae8363a0c33de021484f4`.
Backend reference: [postfiatorg/tasknodeofficial at 41bb946](https://github.com/postfiatorg/tasknodeofficial/tree/41bb94647cd2e6183d0cbe193b568d179c79a9b6).

**Classification: bounded fix.** Product specification heading: **Shipping MVP — LIVE**.
Requirement excerpt: “Task Node and identity: Tasks, evidence, verification,
rewards, balances, chat, context, linked identity, and live Task Node-linked Nostr
identity.” This restores faithful evidence serialization in the existing JSON
helper. It adds no task, authorization, payment, storage, or lifecycle surface.

## Reviewed path and contract

The remote MCP server is implemented in Task Node's backend, not in this Rust
repository. Corbanu's MCP handler forwards tool arguments; its Task Node CLI and
TUI are separate adapters to the **same** terminal backend routes used by MCP.
The fixes here are in Corbanu's evidence-input adapter. They do not claim to
modify the remote MCP server.

1. `codex-rs/cli/src/tasknode_cmd.rs`: artifact parsing, type inference, summary
   URL extraction, lifecycle preflight and POST to `/api/terminal/tasknode/tasks/<id>/evidence`.
2. `codex-rs/tui/src/chatwidget/tasknode_menu.rs`: separate summary-only adapter.
3. Backend `server/tasknode-mcp.js`: four advertised artifact types (`text`,
   `url`, `github_pr`, `git_commit`), descriptive maximum of two attachments;
   tools re-enter terminal routes. The CLI's explicit metadata types are more
   permissive; that alone is not evidence of a rejection defect.
4. Backend `server/tasknode-terminal-routes.js` and
   `server/tasknode-terminal-evidence.js`: 1 MiB evidence request limit,
   URL/value/text compatibility, summary prepended as the primary evidence item.
5. Backend `server/task-submission.js` and `server/offchain-task-lifecycle.js`:
   task/account/lifecycle gates, direct-write evidence normalization and event
   persistence. Evidence selectors keep two items; values are bounded to
   120,000 characters and notes to 8,000. No database write was performed here.
6. Backend `server/task-review-evidence.js`: processed-evidence selection and
   URL retrieval. `url` invokes excerpt retrieval; `github_pr`/`git_commit` are
   retained as provided text by this function. Retrieval was stubbed in the
   offline probe; live reviewer processing was not exercised.

## Confirmed findings

| ID | Reproduction and impact | Disposition |
| --- | --- | --- |
| E1 | Bare `https://example.test/report?a=1&b=two` is split at `=` into type `https://example.test/report?a` and value `1&b=two`. The intended URL is lost before the request reaches Task Node. | **Fixed**: recognize an HTTP(S) scheme before considering `type=value`; retain the entire input, including case-insensitive schemes. Explicit typed artifacts still split only once. |
| E2 | `https://notgithub.com/owner/repo/pull/42`, a redirect URL containing a GitHub URL in its query, and a GitHub issue containing `/pull/` in its query are mislabeled as GitHub artifacts by substring matching. Incorrect `github_pr`/`git_commit` metadata also bypasses the reviewer's `url` retrieval branch. | **Fixed in the CLI**: parse the actual host and owner/repository/resource path. Generic HTTP(S) resources remain `url`; non-URL notes remain `text`. |
| E2-TUI | The unchanged TUI helper emits `github_pr` for the same `notgithub.com` URL. Its exact-source regression probe fails with expected `url`, observed `github_pr`. | **Unfixed**: separate interactive adapter. This PR is confined to the noninteractive CLI; a TUI fix needs its applicable interactive qualification. |
| E3 | A report plus two advertised attachments becomes three items in `terminalTaskEvidenceSubmission`; `directEvidenceItemsFromPayload` keeps only the report and attachment one. Attachment two is dropped before the direct-write event payload is built. | **Unfixed upstream**: backend storage contract belongs to `postfiatorg/tasknodeofficial`. The included offline backend probe reproduces the selector without submitting a task or accessing a database. |

Summary punctuation/Markdown handling was inspected but not changed: the
existing whitespace extractor has no documented Markdown or punctuation
boundary policy. Treat its ambiguity as a follow-up, not a proven contract fix.
This review does not claim that every possible backend defect has been found.

## Regression evidence and reproduction

Production regression tests are in the CLI binary's `tasknode_cmd::tests`:

- `bare_artifact_urls_preserve_equals_signs`: HTTP, HTTPS, mixed-case schemes,
  query/path/fragment preservation, actual GitHub PRs, explicit `url=...`.
- `artifact_type_uses_the_actual_github_host_and_resource_path`: exact payloads
  for misleading host/query/path matches and existing genuine PR/commit behavior.

The focused probe extracts the **actual** helper functions and the same tests;
it reimplements no production parser. Use the repository's isolated wrapper,
which supplies an empty profile and disables native keyring access:

```sh
python qa/reliability/tasknode-evidence-contract-probe.py --baseline 4f09d7af996b9b5fb68ae8363a0c33de021484f4
python scripts/isolated_rust_tests.py --manifest-path ../qa/artifacts/tasknode-evidence-contract/baseline/Cargo.toml -p tasknode-evidence-contract-probe --lib
# Observed: 2 run, 0 passed, 2 failed (E1 and E2).
python qa/reliability/tasknode-evidence-contract-probe.py
python scripts/isolated_rust_tests.py --manifest-path ../qa/artifacts/tasknode-evidence-contract/fixed/Cargo.toml -p tasknode-evidence-contract-probe --lib
# Observed: 2 run, 2 passed, 0 skipped.
python qa/reliability/tasknode-evidence-contract-probe.py --tui
python scripts/isolated_rust_tests.py --manifest-path ../qa/artifacts/tasknode-evidence-contract/tui-unfixed/Cargo.toml -p tasknode-evidence-contract-probe --lib
# Observed: 1 failed; records the explicitly unfixed E2-TUI.
```

For E3, obtain the pinned backend files `task-review-core.js`,
`tasknode-terminal-evidence.js`, `offchain-task-lifecycle.js`, and
`task-review-evidence.js` from the referenced commit's `server/` directory:

```sh
node qa/reliability/tasknode-evidence-backend-probe.mjs /path/to/pinned/server
# CONFIRMED: summary + 2 attachments -> 3 inputs -> only 2 selected.
# CONFIRMED: incorrect github_pr skips URL retrieval; url invokes it (stubbed).
```

The backend probe evaluates exact extracted functions with synthetic inputs;
its HTTP dependency is stubbed and it has no credential or database access.
The ordinary full-package command is:

```sh
just test -p codex-cli --bin codex -E 'test(tasknode_cmd::tests::)' --locked
```

The scoped CLI binary run passed **12 tests, 0 failures, 260 unrelated tests
filtered out**, on Windows with the pinned Rust 1.95.0 toolchain. PowerShell 7
was unavailable to the Windows `just` recipe, so the exact recipe's Python
isolation wrapper was invoked directly with the arguments shown above. This
tests the actual CLI binary module, including existing profile/lifecycle tests.
It does not claim a production API write or packaged-application qualification.
Formatting uses the pinned Rust 1.95.0 toolchain; `git diff --check` passes.
Independent code-blind input cases were frozen before review of results. The
independent review found case-sensitive scheme handling; it was corrected and
covered before the final red/green run. No TUI flow or release binary was changed
or handed off as qualified. Upstream maintainer review and CI remain separate.
