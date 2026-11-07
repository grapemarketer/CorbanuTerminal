---
name: tasknode-usage
description: Use Task Node as an agent-side second brain and work ledger. Use when the user asks Codex to interact with Task Node, request or accept tasks, submit task evidence, answer verification requests, inspect rewards or balances, use Task Node chat for context-aware judgment, read or edit the Task Node context document, or when Codex is uncertain about a user-specific choice and Task Node chat can resolve it without asking the user.
---

# Task Node Usage

## Role

Use Task Node to drive clarity for the user while working inside multifaceted AI systems. The web UI is human-facing; the terminal bridge is agent-facing. Act on behalf of the user, keep state coherent, and use Task Node when it can reduce ambiguity or preserve durable context.

This skill is for using Task Node. For implementation work inside `/home/pfrpc/repos/tasknodeofficial`, also use the `tasknodeofficial` skill.

## Core Principles

- Preserve the terminal-provided `CORBANU_TASKNODE_PROFILE` and `CODEX_HOME`. Missing or conflicting scope must stop account operations; never guess the default account or retry with another profile.

- Treat Task Node chat as a context-aware second brain. It often knows the user's recent work, preferences, and durable operating context better than the current thread.
- Prefer Task Node chat over asking the user when the uncertainty is about user-specific priorities, project direction, or historical context and the action is reversible.
- Ask the user directly when the choice is high-stakes, irreversible, explicitly personal, or Task Node lacks enough context.
- Keep all Task Node writes honest. Never claim work is complete unless it is complete and verified.
- Do not paste secrets, bearer tokens, private keys, or unrelated logs into Task Node chat, task requests, context, or evidence.
- Optimize for full reward by doing the actual work, verifying it, and submitting audit-friendly evidence.

## Task Node Chat

Use chat when choosing between plausible approaches, interpreting the user's durable priorities, or answering "what does Task Node think?" prompts. Default to the Task Node terminal chat route, which uses Private Thinking by default.

Good chat prompts include:

- the decision to make;
- the current work context;
- the options under consideration;
- constraints and risks;
- the exact form of answer needed.

After using chat, continue the work. Summarize Task Node's visible answer only when it affects the user-facing decision or the user asked to see it. Do not invent or expose hidden reasoning.

## Collaborator Context

When the user needs collaborators' shared work context, read `"$CORBANU_BIN" tasknode team context --json` after the helper-resolution preamble and the Team Context capability check in `references/tooling.md`. The capability check handles an older installed release alongside a newer debug binary without changing account scope. It exposes the existing web Team Context report with the same account and task-history permissions. Preserve reported freshness and sharing limits. The user's `context get` document is a separate surface; do not conclude that Team Context is unavailable just because it is absent from that document.

## Context Document

Treat the context document as the user's operating manual. It must stay human-readable, concise, and useful for future agents.

Update it only for durable context:

- stable user preferences;
- current strategic direction;
- recurring project facts;
- operating constraints;
- lessons that should affect future Task Node chat or task generation.

Do not add machine sludge: raw logs, giant transcripts, temporary implementation traces, speculative notes, or verbose summaries of one-off work.

When editing context:

1. Read the current document first.
2. Make the smallest durable edit that improves future work.
3. Preserve human structure and headings.
4. Save through Task Node tooling.
5. Show the user an inline diff of what changed and state that it was saved.

## Task Lifecycle

Task Node work usually follows this path:

1. A task request is submitted as a user-defined text blob.
2. Task Node generates a task.
3. The task is accepted or refused.
4. Accepted work appears as outstanding accepted work.
5. Initial evidence is submitted after real work is done.
6. The task is re-read and the normal verification request is collected.
7. The verification response is submitted honestly and thoroughly.
8. The task is re-read until Task Node reports a reward outcome or another explicit state.

When requesting a task, include enough relevant context to let Task Node scope it well. Do not submit a vague request unless the user explicitly asks for open-ended task generation.

When accepting a task, inspect the full card before acting. Confirm objective, steps, reward, deadline, verification criteria, and current status.

Before the first Task Node shell command, use the fail-closed helper-resolution preamble in references/tooling.md. It preserves this session's effective `CODEX_HOME`, prefers an installed Corbanu entrypoint, and uses a legacy PFTerminal alias only as a compatibility fallback. Never compile a terminal binary from source just to obtain the Task Node helper. A command that loses the session home reads a different vault and can falsely report "not linked".

When submitting initial evidence for an accepted task, first inspect the task with `"$CORBANU_BIN" tasknode task show <task-id> --json` and confirm `actions.canSubmitInitialEvidence` is true. Draft the response from the Initial Evidence template, save it to a temporary file, and submit it with `"$CORBANU_BIN" tasknode task evidence <task-id> --body-file <path> --json`. Do not use the verification response command for this state.

Initial evidence is not completion. After every initial evidence receipt:

1. Read `pfterminalLifecycle` in the receipt and run its `nextCommand`. Keep its `submissionRef`. If the command instead fails with `task_evidence_receipt_unconfirmed`, do not resubmit blindly; follow its `nextCommand` and message.
2. Re-read the task with `"$CORBANU_BIN" tasknode task show <task-id> --json`.
3. If `actions.canSubmitVerificationEvidence` is true, answer `currentVerificationRequest` using the Verification Response template and submit it with `"$CORBANU_BIN" tasknode verification respond <task-id> --body-file <path> --json`.
4. Re-read the task after the verification response. Claim completion only when `rewardOutcome` exists or the task is explicitly `rewarded`.

Verification generation can be asynchronous. If the task is still submitted but has no verification request yet, keep it classified as awaiting verification, check `"$CORBANU_BIN" tasknode tasks list --tab verification --json` before ending the Task Node work cycle, and do not report it as complete. At the beginning of later Task Node work, drain any pending verification requests before starting new submissions. If a task stays submitted with no verification request across later work cycles, report the task id and `submissionRef` to Task Node operators; never cancel it to get a fresh evidence step.

When submitting evidence, make it easy for the verifier to prove the work happened. Include artifacts, exact commands, test output summaries, PRs, commits, screenshots, route probes, or file references as applicable.

The submission parser scans evidence prose for URLs and registers each one it finds as a formal evidence item. Because of this, only cite durable, reviewable artifacts in the evidence body: real file paths, commit hashes, task IDs, event IDs, or production URLs. Never embed transient or non-reviewable endpoints — localhost, 127.0.0.1, deliberately-broken origins (for example an invalid host used to trigger a failure), or throwaway stub-server URLs. Describe test setup by the exact command run and the observed output, not by citing the throwaway endpoint. A localhost URL registered as an evidence item is worthless to a reviewer and cannot be removed after submission.

When responding to verification, first confirm `actions.canSubmitVerificationEvidence` is true. Answer the specific verifier request with `"$CORBANU_BIN" tasknode verification respond <task-id> --body-file <path> --json`. If the verifier asks for a complete generated text, a pass/fail summary, a missing artifact, or a clearer proof point, provide exactly that. Do not dodge, summarize away required detail, or claim success if the work failed.

## Reward Standard

The goal is the highest honest reward. A partial or missing reward usually means the evidence did not prove completion, the task requirements were not fully satisfied, or shortcuts were taken.

An evidence receipt or verification-response receipt proves only that a lifecycle write was accepted. It is not a reward receipt. Treat `rewardOutcome` or an explicit `rewarded` task state as the completion boundary.

Evidence and verification responses should be concise but complete:

- state what was done;
- cite artifacts;
- state what was verified;
- map the proof to the task requirements;
- disclose residual risk or incomplete items.

Prefer strong evidence over long prose.

## Tooling References

Before making live Task Node changes or operating through the terminal bridge, read [tooling.md](references/tooling.md).

Before drafting task requests, evidence, verification responses, or context-document diffs, read [submission-templates.md](references/submission-templates.md).
