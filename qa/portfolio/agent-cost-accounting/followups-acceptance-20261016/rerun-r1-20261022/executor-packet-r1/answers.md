# Integrator's answers to the designer's questions (facility facts only), 2026-10-16

Written by the integrator after the designer's proposal was saved unchanged (`designer-output.md`). These are facts
about the test environment and the product's controls; they add no expected results. Where the harness was extended
to answer a question, the extension is described here and will be in the tester's `agms-tui help`.

1. **Leaving `/side`:** Ctrl+C (`agms-tui key C-c`) returns from a side conversation to the saved conversation; the
   product says so on screen. (Esc in an empty composer inside `/side` does not leave it.)
2. **Automatic reviewer:** start the product with
   `-c 'approvals_reviewer="auto_review"' -c 'approval_policy="on-request"' -c 'sandbox_mode="read-only"'`.
   With these, a command that writes in the workspace needs approval and the approval goes to the automatic reviewer.
3. **`fork:`:** the text UI offers no temporary fork other than `/side`, and the harness has no other client that makes
   one. `fork:` can't be exercised in this run (named limit).
4. **No-charge fallback:** new control `agms-tui standin no-charge on|off`. With `on`, the charging stand-ins
   (OpenRouter, Vercel, Corbanu API) answer with the same usage but leave out the charge field, until `off`. Two more
   OpenRouter models were added: `xiaomi/mimo-v2.6-pro` (always reports 1,200 input, 30 output, no charge) and
   `standin/uncatalogued` (900 input, 15 output, no charge). Whether the product's catalogue has a rate for any
   OpenRouter model is not stated in the packet; the tester can't know it in advance.
5. **Routes within one profile:** yes. Stop the product and start it again with another `--route`/`--model` on the
   same profile; conversations from earlier routes stay in that profile.
6. **Provider name:** new option `agms-tui start --route moonshot --provider-name NAME` (lowercase letters, digits,
   `-`). The default is `kimi`. Each model still gets one first turn only, so name-independence can be checked by
   giving the second model's conversation a different name (no extra spend).
7. **`config.toml`:** new control `agms-tui config 'skip_reopen_warmup = true'` appends that line to the profile's
   `config.toml` (takes effect at the next start). There is no "remove" control; a later `-c skip_reopen_warmup=false`
   at start overrides it.
8–14. No further information beyond the packet; keep your stated assumptions, or mark the expected result as
   observational.
15. **Credentials:** the tester never sees real keys; the product only holds synthetic placeholders (values starting
   `agms-synthetic`). The integrator scans all evidence for the real key values afterwards. The tester's pattern scan
   stays as designed (advisory).

Other facts the cases rely on:

- **Timing:** `/cost` can read the ledger a moment before the latest request is written. Its page has a `Refresh`
  entry; re-opening `/cost` also re-reads.
- **Exec:** `agms-tui exec [--route R --model M] [--ephemeral] 'prompt'` runs `corbanu exec` on the same profile and
  project and prints its output (route `zai` by default).
- **Moonshot limit:** the relay refuses Moonshot requests after the 12th in the whole run (owner's spend limit),
  answering "test harness: Moonshot request limit reached".
- **Group profiles:** each group gets a fresh profile and project; nothing carries over between groups.
