// Offline contract probe of exact public Task Node normalization/reviewer code.
// Argument: a directory containing the pinned backend files listed in the report.
// No production submission, database, wallet, credentials, DNS, or HTTP requests.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import vm from "node:vm";

const dir = process.argv[2];
if (!dir) throw new Error("Pass the pinned backend source directory");
const read = name => readFileSync(join(dir, name), "utf8");
const between = (source, start, end) => {
  const first = source.indexOf(start);
  const last = source.indexOf(end, first + start.length);
  assert(first >= 0 && last > first, `Source markers missing: ${start}`);
  return source.slice(first, last).replaceAll("export ", "");
};
const helpers = between(read("task-review-core.js"), "export function safeText", "export function stableJson");
const normalize = read("tasknode-terminal-evidence.js").replace("export ", "");
const select = between(read("offchain-task-lifecycle.js"), "function directEvidenceItem(", "function normalizeDirectSubmissionPayload(");
const review = between(read("task-review-evidence.js"), "export async function processedEvidenceFromPayload(", "export function artifactUrl(");
const calls = [];
const context = {
  fetchUrlExcerpt: async url => {
    calls.push(url);
    return {status: "fetched", url, excerpt: "Synthetic public page"};
  },
};
vm.runInNewContext(helpers + normalize + select + review + `
globalThis.api = {terminalTaskEvidenceSubmission, directEvidenceItemsFromPayload, processedEvidenceFromPayload};
`, context, {timeout: 1000});
const { api } = context;
const payload = api.terminalTaskEvidenceSubmission({summary: "Review report", evidence: [
  {type: "url", value: "https://example.test/one"},
  {type: "url", value: "https://example.test/two"},
]}, "synthetic_task");
assert.equal(payload.evidence_items.length, 3);
const selected = api.directEvidenceItemsFromPayload(payload);
assert.deepEqual(Array.from(selected, item => item.value), ["Review report", "https://example.test/one"]);
console.log("CONFIRMED upstream: report + 2 advertised artifacts becomes 3 items; persistence selector drops artifact 2.");
await api.processedEvidenceFromPayload({evidence_items: [{artifact_type: "github_pr", value: "https://example.test/report"}]});
assert.equal(calls.length, 0);
await api.processedEvidenceFromPayload({evidence_items: [{artifact_type: "url", value: "https://example.test/report"}]});
assert.deepEqual(calls, ["https://example.test/report"]);
console.log("CONFIRMED reviewer branch: github_pr is provided text; url invokes URL retrieval (HTTP stubbed).");
