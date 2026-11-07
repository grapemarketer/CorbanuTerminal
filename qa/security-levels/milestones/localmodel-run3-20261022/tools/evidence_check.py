#!/usr/bin/env python3
"""qa-local3 integrator: AGG-20P evidence check (amendment 7). usage: evidence_check.py PRODUCT_HOME
For every session file: name, SHA-256, session_meta (source, agent_role, nickname, parent), whether its model instructions
carry the Aggressive level paragraph (the sentence beginning "The person chose security level ..."), and every spawn call
with its agent_type/model and what the product returned."""
import glob, hashlib, json, os, re, sys
for f in sorted(glob.glob(sys.argv[1] + "/sessions/**/*.jsonl", recursive=True)):
    raw = open(f, "rb").read()
    print("\n## %s  sha256 %s" % (os.path.basename(f), hashlib.sha256(raw).hexdigest()))
    calls, level, turns = {}, None, 0
    for line in raw.decode().splitlines():
        r = json.loads(line)
        t, p = r.get("type"), r.get("payload", {})
        if t == "session_meta":
            src = p.get("source")
            print("  meta: id=%s source=%s agent_role=%s nickname=%s" % (p.get("id"), json.dumps(src)[:200], p.get("agent_role"), p.get("agent_nickname")))
        if t == "turn_context":
            turns += 1
        if t == "response_item" and p.get("role") in ("developer", "system") and level is None:
            m = re.search(r"The person chose security level (\w+)[^\"]{0,300}", json.dumps(p))
            if m: level = m.group(0)[:320]
        if t == "response_item" and p.get("type") == "function_call" and p.get("name", "").startswith("spawn_agent"):
            calls[p["call_id"]] = p.get("arguments")
        if t == "response_item" and p.get("type") == "function_call_output" and p.get("call_id") in calls:
            print("  spawn call: %s\n    returned: %s" % (calls.pop(p["call_id"]), str(p.get("output"))[:400]))
    print("  turns: %d" % turns)
    print("  level paragraph: %s" % (level or "none (no model turn in this session)"))
