**(c) Open questions that remain**

- **Q1 (owner; D136, NA-07).** An account is removed by another process while a session runs on it. May that session's next request still succeed on the credentials it holds in memory (`t1`'s sign-in, `work`'s key), as long as the user is told? This is recorded as "observe" for now.
- **Q2 (owner; ESC-03, REG-01c).** Is there a supported way to open `/resume` with a draft in the composer? If not, does the draft clause apply?
- **Q3 (owner; AC6).** Some keys are typed or passed openly:
  - Should a key typed at the `account add` stdin prompt, in the `/providers` Z.AI key field or in onboarding be masked or hidden?
  - Is a key given on the command line and repeated back in a usage error a violation?
- **Q4 (owner; AC8).** Expected feature-off behaviour of:
  - `exec --account work` and `account list`;
  - Esc in the `corbanu resume` picker when credentials exist (exit, or a new session);
  - whether `zainokeyoff` should still show onboarding.
- **Q5 (owner).** What should `[provider_accounts]` contain after its account is removed through `/providers` (NA-07)?
- **Q6 (owner).** The footer reads `via zai default` while `/status` names another account (REF-01/02/04/09). Is that intended?
- **Q7 (not testable here).**
  - D124's "vault cannot be read" path.
  - Completing a device-code sign-in.
  - The CLI `login --account <name> --device-auth` first screen. It was left out because a cancelled attempt might leave a stub account in `oai1`; add it only if the coordinator agrees.
  - Live token refresh (D135) and Bedrock (D99) are out of scope.
- **Q8 (coordinator).** Review-facts item 7 says "remove it at most once and do so last". This design runs ST-03 after the removal. ST-03 does not use `t1` but needs the home without it. Confirm that this is acceptable; otherwise ST-03 is dropped and D86's "no line for the default OpenAI login" goes untested.
- **Q9 (observe).** Which model fresh-home onboarding picks (ESC-04).
- **Q10 (observe).** Whether spawning is available by default (NA-09). If it is not, AC4's spawned-agent leg remains uncovered.

VERDICT: CHANGES
