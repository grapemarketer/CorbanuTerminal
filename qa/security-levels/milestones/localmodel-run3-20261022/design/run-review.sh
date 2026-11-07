#!/bin/bash
D=/Volumes/CorbanuDrive/Corbanu/.codex-work/qa-local3-20261022/design
date -u +"start %FT%TZ" > $D/review-start.txt
~/.local/bin/corbanu exec --json -s read-only --skip-git-repo-check -m claude-opus-5-5-plan -c model_provider="claude-plan" -c model_reasoning_effort="high" -C $D/review-packet -o $D/review-output.md - < $D/review-prompt.md > $D/review-events.jsonl 2> $D/review-stderr.txt
echo "exit=$?" >> $D/review-start.txt
date -u +"end %FT%TZ" > $D/review-end.txt
