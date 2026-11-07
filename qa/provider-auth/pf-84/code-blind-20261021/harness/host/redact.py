"""stdin -> stdout redaction for the record: device codes, email hints, guest run paths, the host work path."""
import re
import sys

Q = "/Volumes/CorbanuDrive/Corbanu/.codex-work/pf84-cb1-20261021"
text = sys.stdin.read()


def code(m):
    t = m.group(0)
    return "<CODE>" if re.search(r"[0-9]", t) and re.search(r"[A-Z]", t) else t


text = re.sub(r"(?<![A-Za-z0-9])[A-Z0-9]{4}-[A-Z0-9]{5}(?![A-Za-z0-9])", code, text)
text = re.sub(r"[A-Za-z0-9._%+-]*(?:\u2026|\\u2026)@[A-Za-z0-9.-]+", "<email-hint>", text)
text = re.sub(r"[A-Za-z0-9._%+-]+@gmail\.com", "<email>", text)
text = re.sub(r"/Users/agmx[0-9]/runs/agms-[A-Za-z0-9-]+", "<RUN>", text)
text = text.replace(Q, "<Q>")
sys.stdout.write(text)
