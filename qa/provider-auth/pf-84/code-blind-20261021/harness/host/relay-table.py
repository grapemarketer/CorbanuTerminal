"""usage: relay-table.py MEDIATOR_JSONL [FROM_UTC TO_UTC]
Prints the product's requests (not the tester's) as: UTC time, target, path, status, key label, model.
Times in the mediator log are the guest's local time with offset; they are converted to UTC."""
import datetime
import json
import sys

rows = []
for line in open(sys.argv[1]):
    try:
        r = json.loads(line)
    except ValueError:
        continue
    if r.get("event") not in ("forwarded", "answered_401", "passthrough", "passthrough_refused", "passthrough_error",
                              "denied_proxy_request", "denied_path"):
        continue
    if r.get("role") == "executor" or r.get("key_label") == "placeholder":
        continue
    t = datetime.datetime.fromtimestamp(r["time_ns"] / 1e9, datetime.timezone.utc).strftime("%H:%M:%S")
    rows.append((t, r.get("event"), r.get("target", ""), (r.get("path") or r.get("request_line") or "")[:40],
                 r.get("status", ""), r.get("key_label", ""), r.get("model") or "",
                 r.get("bytes_down", "")))
lo, hi = (sys.argv[2], sys.argv[3]) if len(sys.argv) > 3 else ("00:00:00", "99:99:99")
for row in rows:
    if lo <= row[0] <= hi:
        print("  ".join(str(x) for x in row))
