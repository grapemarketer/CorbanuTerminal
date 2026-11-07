#!/bin/bash
D=/Volumes/CorbanuDrive/Corbanu/.codex-work/qa-local3-20261022/design
date -u +"start %FT%TZ" > $D/designer-start.txt
~/.local/bin/corbanu exec --json -s read-only --skip-git-repo-check -m claude-opus-5-5-plan -c model_provider="claude-plan" -c model_reasoning_effort="high" -C $D/designer-packet -o $D/designer-output.md - < $D/designer-prompt.md > $D/designer-events.jsonl 2> $D/designer-stderr.txt
echo "exit=$?" >> $D/designer-start.txt
date -u +"end %FT%TZ" > $D/designer-end.txt
