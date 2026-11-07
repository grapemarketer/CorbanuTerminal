"""usage: gate_ok.py EVIDENCE_DIR -> exit 0 only if this clone's current gate run passed: the probe summary's top-level
"ok" is true, the smoke turn's agent_message is exactly AGMS-SMOKE-OK, and both relay positive controls (Z.AI and
OpenAI model listings through the guest mediator and the host relays) returned 200. From pf84-cb1's gate_ok.py."""
import json
import pathlib
import sys

g = pathlib.Path(sys.argv[1]) / "gate"
run = (g / "current").read_text().strip()
summary = json.loads((g / f"{run}.summary").read_text())
smoke = [json.loads(line) for line in (g / f"smoke-{run}.jsonl").read_text().splitlines() if line.startswith("{")]
said = [e["item"].get("text", "").strip() for e in smoke
        if e.get("type") == "item.completed" and e.get("item", {}).get("type") == "agent_message"]
relay = (g / "relay-positive-controls.txt").read_text().splitlines()
want = ["https://api.z.ai/api/paas/v4/models 200", "https://api.openai.com/v1/models 200"]
rel = {w: w in relay for w in want}
ok = summary.get("ok") is True and "AGMS-SMOKE-OK" in said and all(rel.values())
print(f"gate {run}: probes ok={summary.get('ok')} count={summary.get('probe_count')} smoke={said} relay200={rel} -> {'PASS' if ok else 'FAIL'}")
sys.exit(0 if ok else 1)
