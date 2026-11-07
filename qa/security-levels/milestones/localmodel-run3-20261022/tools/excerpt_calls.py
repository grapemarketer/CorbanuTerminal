#!/usr/bin/env python3
"""usage: excerpt_calls.py PRODUCT_HOME : every tool call in the product's session files with the output the product
returned to the model (outputs cut to 1,500 characters). Integrator's read after the run."""
import glob, json, os, sys
for f in sorted(glob.glob(sys.argv[1] + "/sessions/**/*.jsonl", recursive=True)):
    calls = {}
    print("\n## session %s" % os.path.basename(f))
    for line in open(f):
        r = json.loads(line)
        if r.get("type") != "response_item":
            continue
        p = r["payload"]
        if p.get("type") in ("function_call", "custom_tool_call"):
            calls[p["call_id"]] = (r["timestamp"], p.get("name"), p.get("arguments", p.get("input", "")))
        elif p.get("type") in ("function_call_output", "custom_tool_call_output") and p.get("call_id") in calls:
            t, n, a = calls.pop(p["call_id"])
            o = p.get("output")
            o = o if isinstance(o, str) else json.dumps(o)
            print("\n%s %s %s\n  output: %s" % (t, n, a, o[:1500].replace("\n", "\n          ")))
