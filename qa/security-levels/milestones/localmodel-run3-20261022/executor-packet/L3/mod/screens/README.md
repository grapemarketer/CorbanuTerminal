# Screens of this candidate

Text copies of the terminal screen of the candidate (Corbanu Terminal 0.1.48), captured on 2026-10-14 between
14:20Z and 14:34Z on a Mac with a disposable profile and the test project, GLM 5.3 Flash on `zai`. Folder paths are
shortened to `/Users/tester/run`. The test machine's own screens look the same except for paths. These are
examples of real behaviour, not test cases; the model's own words vary between runs.

Session 1 (the security features on, starting at Permissive):

| File | What it shows |
| --- | --- |
| 00-start | First start in the project: trust prompt |
| 01-fresh-start | Fresh session, Permissive |
| 02-security-picker-permissive | `/security` while Permissive |
| 03-picker-moderate-highlight | Moderate highlighted |
| 04-moderate-review | Enter on Moderate: the review before confirming |
| 05-moderate-saved | After Enter: saved, restart offered |
| 06-after-restart | After pressing `r` |
| 07-picker-moderate-active | `/security` after the restart |
| 08-inspector-moderate, 08b-…-scrolled, 08c-…-bottom | Inspector (`i`) under Moderate, top, middle and bottom |
| 09-grants-killswitch | `g`: grants and kill switch |
| 10-outside-write | Asking for a write to `../outside-notes.txt` (outside the project) |
| 11a-escalation-prompt, 11b-…-approved-result | Asking for the same command with a request to run outside the sandbox: the prompt, then the result after Enter on option 1 |
| 12-read-readme | Asking the agent to read README.md (tool output enters the session) |
| 13a-protected-after-untrusted-prompt, 13b-…-result | Then asking for `corbanu vault list`: the prompt, then the result after approving |
| 14a-credential-read-prompt, 14b-…-approved-result | Asking for `cat ~/.ssh/config` (a synthetic file in the test home): the prompt, then the result after approving |
| 15a-corbanu-config-read, 15b-…-result | Asking to `cat` Corbanu's own config file: the prompt, then the result after approving |
| 16-review-aggressive-from-moderate, 16b-esc | `/security`, Aggressive chosen while Moderate: the review, then Esc |
| 17-review-permissive-from-moderate | Permissive chosen while Moderate: the review |
| 18-permissive-saved-from-moderate | After Enter on that review |
| 19-model-connections | `/model` in that session |

Session 2 (the same profile, but the untrusted-content labels feature, `source_envelopes`, switched off):

| File | What it shows |
| --- | --- |
| E1a-moderate-highlight-no-labels | Moderate highlighted |
| E1b-moderate-no-labels | Enter on Moderate: the review (top part) |

Session 3 (security features on):

| File | What it shows |
| --- | --- |
| 21-killswitch-review-moderate | After choosing Moderate, confirming and `r`: `/security` → `g` → "Turn the kill switch on" → Enter (the review; then Esc) |
| E2a-start-plain-http, E2b-broker-refusal | Quit and start again under Moderate with a test connection whose provider address is plain HTTP (a URL the credential broker cannot accept), then a message |
| 22-relaunch-moderate-ordinary-command | Quit and start again under Moderate on `zai`, then an ordinary command in the project |
