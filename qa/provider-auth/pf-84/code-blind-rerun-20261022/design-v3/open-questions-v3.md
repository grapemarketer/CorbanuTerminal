1. **D136, signing in again.** Decision 1 says that if the account is added or signed in again, the session uses the new sign-in. No case can test this: the tester cannot sign in a ChatGPT account, and `t1` may be removed only once.
2. **D136, relay evidence.** The relay cannot see whether chatgpt.com traffic carried `t1`'s sign-in. If the build revokes the token when it signs out, a request wrongly sent on the old sign-in would fail with 401, and its error might look like a refusal. Should the integrator treat traffic in the no-use window that is about the size of `beforeone`'s as a blocker? I kept it advisory.
3. **D136, provider line.** It is not settled whether the line still names `t1` next to "unavailable". Only "unavailable" is required.
4. **NA-06, the add moving the session.** It is not settled whether adding an account in `/providers` may move the session onto it. If it does, Rename is refused ("This session uses zai account `added` …") and the rename results fail with "rename field not shown". The owner should say whether that counts as a product failure.
5. **Not re-run:**
   - ST-03: D136's step 8 and its Files checks cover the "nothing re-created `t1`" check.
   - PR-01 and GH-02: their `[observe]` legs (pq, g6) can be read against decisions 2–3, but their text is unchanged.
   - Decision 2's `/providers` path ("Start a new session on this account", then `/resume`): no case tests it.

