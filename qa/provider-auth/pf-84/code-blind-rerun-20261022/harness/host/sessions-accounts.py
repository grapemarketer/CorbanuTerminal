"""Per conversation file: session id, then each turn's time, provider and account (turn_context records)."""
import json, sys, glob, os
for f in sorted(glob.glob(os.path.join(sys.argv[1], '**', 'rollout-*.jsonl'), recursive=True)):
    print('== ' + os.path.basename(f))
    for l in open(f):
        try: e = json.loads(l)
        except ValueError: continue
        p = e.get('payload') or {}
        if e.get('type') == 'turn_context' and isinstance(p, dict):
            print('  %s turn provider=%s account=%s' % (e.get('timestamp', '')[11:19], p.get('model_provider'), p.get('provider_account')))
