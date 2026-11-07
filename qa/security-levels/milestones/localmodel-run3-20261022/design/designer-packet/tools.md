# Test machine and product facts for amendment L3 (integrator facts, "F")

## The test machine (as in the second local-model run, with two harness changes)

- One fresh Tart clone of the test machine per group, one at a time. The tester is the candidate's `corbanu exec` on
  GLM 5.3 Flash (Z.AI), in an unprivileged account; its writable area is `$AGMS_RUN`. The tester prompts of earlier
  runs (`examples/`) show its Environment section: `agms-tui start|text|key|screen|wait|fixture|stop`.
- `agms-tui start --route qwen` launches the product on the owner's local model
  `Qwen3.8-Flash-Next-Uncensored-MLX-Serve-mixed-4-8bit` (connection `qwen`; `--model qwen-local` is implied). A `r`
  restart keeps the same options. `--route zai` (GLM 5.3 Flash) and `--model glm-5.2` still exist but are not this
  run's product model. In the product's model lists the local model shows as "… via qwen (current)" under "Configured
  custom providers", with a "Z.AI [Other]" tab next to it (screens-v7 M04).
- The local model answers one request at a time; a parent pane, a child pane and a sub-agent wait for each other.
  Replies take about 10-80 s; the first turn of a session, and turns with a long context, can take 3-5 minutes (one
  shakedown turn took about 4 minutes). On every turn the product shows "⚠ Model metadata for `Qwen3.8-…` not found …"
  (screens-v6 07), which is expected.
- The product's home is `$AGMS_RUN/product-home` (`$CORBANU_HOME`); its `config.toml` turns the security features on
  (protected launch included) and starts at Permissive. A saved level is kept in `security_level.toml` there; after
  a level is saved the home also holds `security_state.json` (seen in the shakedown). The product's `~` is the tester's
  `$HOME`, wiped after the run.
- **Harness change 1 (this run):** in the AGG-20P groups (L03, L04) the product's `config.toml` is writable by the
  tester (mode 600, as a person's own file is), so amendment 7's shell blocks can append to it. In the MOD groups (L01,
  L02) it stays read-only to the tester, as in the earlier Moderate runs (those cases do not edit it). Where it is
  writable, the first start's trust prompt ("1. Yes, continue") adds a `[projects."…/workspace"]` table with
  `trust_level = "trusted"` to it, so later starts in that group show no trust prompt. Saving a level does not change
  `config.toml` (the Aggressive review says "Your config.toml is not modified"); REMOVE-WIDE printed `0` then `1` in the
  shakedown.
- **Harness change 2 (this run):** when the product exits (for example a refused start), its last screen stays shown,
  ending with "Pane is dead (status N, <time>)". `agms-tui start` then refuses ("a product session is already
  running; use 'agms-tui stop' first") until `agms-tui stop` is run. `agms-tui screen` shows only the visible screen:
  the first lines of a long exit message can be scrolled off (screens-v7 P05-P07; the full text is in the run's raw
  terminal log, which the integrator reads afterwards).
- Network: none for agent commands beyond the model routes; `curl https://example.com` from an agent fails.
- The relay and the request logs are not visible to the tester. The integrator reads the product's session files and
  the guest's request log afterwards (for MOD-21 also whether the task reached the child as the person's own input,
  #1175, which the tester does not judge).

## What the shakedown showed (screens-v7)

- **MOD-21 flow on the local model** (M00-M16): Moderate (restarted) works as in the Moderate packet. `/spawn` → Troll →
  "1. Harness: Corbanu Terminal" → the local model "(current)" creates the pane immediately: there is **no reasoning
  screen** and no task box. Spawn status shows "• Burzum [troll]" and "Send task to Burzum [troll]"; the "Send Task to"
  box takes the task; after Enter the main pane shows no message; Status then shows "running; current task: …". The
  child's own view opens with Enter on its row; in that view `/spawn` lists "Quick start" first again and "Spawn
  status" is reached with Down five times. The inspector's "Agents" section lists "main session: Moderate" and
  "agent <id>: Moderate".
- **AGG-20P** (P01-P19):
  - With p20plain and p20wide (W1) defined and the level at Permissive, the product starts normally. The Aggressive
    review names `p20wide` ("Child agents now: … except custom roles that change them: `p20wide`") and the confirm
    saves Aggressive ("Saved: Aggressive").
  - `r` on that screen: the product prints "Restarting Corbanu Terminal to apply the saved security level…", then
    "Security level Aggressive could not be fully applied:", "Child agents: role `p20wide` sets sandbox_mode",
    "Fix the setting, or choose Permissive by editing …/security_level.toml." and exits. Aggressive stays saved.
  - With Aggressive saved, a plain start with W1, W2 or W3 defined does the same (P05-P07; the key named is
    `sandbox_mode`, `approval_policy` or `sandbox_workspace_write`). So in the shakedown REQ-WIDE could never be sent:
    no session with Aggressive active runs while p20wide is defined.
  - After REMOVE-WIDE the product starts with Aggressive active; the inspector shows "Saved: Aggressive".
  - With p20plain only, the model lists `p20plain` among its spawn tool's `agent_type` values (P10). `/spawn` search
    for `p20` shows "no matches" (P11): custom roles are not `/spawn` roles.
  - REQ-PLAIN: the parent called its spawn tool with `agent_type` p20plain (seen afterwards in the session file). The
    "Spawned <id> (<model> medium)" cell does not show the role. The parent's `wait_agent_v1` and `close_agent_v1` calls
    each got an "Allow `…`?" question (Aggressive asks for tools it has not classified after untrusted content). The
    sub-agent's command prompts appear in the main pane with "Thread: Agent (<id>)" (P13, P14). The inspector's
    "Agents" section lists "agent <id>: Aggressive", and "Recent denials" lists denials with "(agent <id>)". `/panes`
    does not list the sub-agent.
