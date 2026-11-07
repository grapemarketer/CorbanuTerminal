#!/bin/bash
# pf84-cb1: utm-par3 provision.sh plus agms-prep. Run as root (via admin.sh). Provisions AGMS (Aggressive milestone, 2026-10-10) code-blind executor
# isolation on the sealed guest "macOS 3" and records every original setting it changes under restore-state/.
# Adapted from PF-83 campaign 35 (pf83-codeblind-35/harness/provision.sh).
echo PF83BEGIN
set -euo pipefail
S=/Users/agent/agms
ST=$S/restore-state
LANES="agmx0 agmx1 agmx2 agmx3"
mkdir -p "$ST"
say() { echo "[provision] $*"; }

# 0. Original state, written once.
if [ ! -f "$ST/recorded" ]; then
  stat -f '%N %Lp %Su:%Sg' /Users/neo /Users/neo_1 /opt/homebrew /opt/homebrew/bin /opt/homebrew/sbin > "$ST/modes.txt"
  cp -p /etc/pf.anchors/corbanu.pf83 "$ST/corbanu.pf83.orig"
  cp -p /etc/hosts "$ST/hosts.orig"
  (mount | grep -i virtiofs || true) > "$ST/virtiofs-mount.txt"
  (stat -f '%N %Lp' /private/tmp/.s.PGSQL.5432 2>/dev/null || true) > "$ST/pgsock-mode.txt"
  pfctl -sI -v 2>/dev/null > "$ST/pf-interfaces.orig" || true
  date -u +%FT%TZ > "$ST/recorded"
fi

# 1. Executor group and accounts: no password, no admin, no SSH, hidden. agmx0 = coordinator capture lane.
dscl . -read /Groups/agmsexec >/dev/null 2>&1 || {
  dscl . -create /Groups/agmsexec; dscl . -create /Groups/agmsexec PrimaryGroupID 610
  dscl . -create /Groups/agmsexec RealName "AGMS code-blind executors"; }
uid=610
for u in $LANES; do
  if ! dscl . -read "/Users/$u" >/dev/null 2>&1; then
    dscl . -create "/Users/$u"
    dscl . -create "/Users/$u" UserShell /bin/bash
    dscl . -create "/Users/$u" RealName "AGMS executor $u"
    dscl . -create "/Users/$u" UniqueID "$uid"
    dscl . -create "/Users/$u" PrimaryGroupID 610
    dscl . -create "/Users/$u" NFSHomeDirectory "/Users/$u"
    dscl . -create "/Users/$u" Password '*'
    dscl . -create "/Users/$u" IsHidden 1
  fi
  mkdir -p "/Users/$u"; chown "$u:agmsexec" "/Users/$u"; chmod 700 "/Users/$u"
  uid=$((uid + 1))
done
printf '%s\n' "agent ALL=(agmx0,agmx1,agmx2,agmx3) NOPASSWD: ALL" > /etc/sudoers.d/agms-codeblind
chmod 440 /etc/sudoers.d/agms-codeblind
visudo -cf /etc/sudoers.d/agms-codeblind

# 2. Read-only package, tools and packet, root-owned.
mkdir -p /opt/agms
chmod -R u+w /opt/agms
rm -R /opt/agms/pkg /opt/agms/bin /opt/agms/etc /opt/agms/packet 2>/dev/null || true
mkdir -p /opt/agms/bin /opt/agms/etc /opt/agms/packet/prompts
cp -R "$S/stage/package" /opt/agms/pkg
cp "$S/stage/package-files.txt" /opt/agms/package-files.txt
cp /Users/agent/bin/tmux /opt/agms/bin/tmux
for f in agms-launch agms-probe agms-product agms-tui agms-mcp-notes agms-prep; do cp "$S/stage/h/$f" /opt/agms/bin/; done
for f in tmux.conf mediation-ca.pem product-config.toml fixture.tar expected.json; do cp "$S/stage/h/$f" /opt/agms/etc/; done
[ -d "$S/stage/packet" ] && cp -R "$S/stage/packet/." /opt/agms/packet/ || true
chown -R root:wheel /opt/agms
find /opt/agms -type f -exec chmod 444 {} +
chmod 555 /opt/agms/bin/*
find /opt/agms/pkg -type f -perm +111 -exec chmod 555 {} + 2>/dev/null || true
chmod 555 /opt/agms/pkg/bin/* /opt/agms/pkg/codex-path/* /opt/agms/pkg/codex-resources/zsh/bin/zsh /opt/agms/pkg/codex-resources/telegram/setup-telegram.sh 2>/dev/null || true
find /opt/agms -type d -exec chmod 555 {} +
chmod 755 /opt/agms
( cd /opt/agms && find pkg -type f | sort | xargs shasum -a 256; shasum -a 256 bin/* etc/* ) > "$S/opt-digests.txt"
find /opt/agms/packet -type f -exec shasum -a 256 {} + >> "$S/opt-digests.txt" || true

# 3. Lock other homes, Homebrew, the unrelated database socket and the host share for the campaign.
chmod 700 /Users/neo /Users/neo_1
# Homebrew tools off the executors' PATH (libraries stay readable: the pinned tmux links one).
chmod 750 /opt/homebrew/bin /opt/homebrew/sbin
chmod 755 /opt/homebrew
[ -S /private/tmp/.s.PGSQL.5432 ] && chmod 700 /private/tmp/.s.PGSQL.5432 || true
if mount | grep -q "My Shared Files"; then umount "/Volumes/My Shared Files" || diskutil unmount force "/Volumes/My Shared Files"; fi

# 4. Per-user egress: executors reach only the loopback mediator.
python3 - "$ST/corbanu.pf83.orig" /etc/pf.anchors/corbanu.pf83 <<'PY'
import sys
src, dst = sys.argv[1], sys.argv[2]
text = open(src).read()
marker = "# 1. inbound SSH from the host only."
block = """# 0. AGMS Aggressive-milestone code-blind executors (2026-10-10): loopback inference mediator only.
pass out quick on lo0 proto tcp from any to 127.0.0.1 port 18443 user { agmx0, agmx1, agmx2, agmx3 } keep state
block drop out log quick proto { tcp, udp } user { agmx0, agmx1, agmx2, agmx3 }

"""
assert marker in text and "AGMS" not in text
text = text.replace("\nset skip on lo0\n", "\n# set skip on lo0   (suspended by AGMS 2026-10-10; restore.sh puts it back)\n", 1)
open(dst, "w").write(text.replace(marker, block + marker, 1))
PY
pfctl -nf /etc/pf.conf
pfctl -f /etc/pf.conf 2>&1 | grep -v -e ALTQ -e "flushing of rules" -e "present in the main" -e "pf.conf for further" -e '^$' || true
pfctl -a corbanu.pf83 -sr
say "lo0: $(pfctl -sI -v 2>/dev/null | grep lo0 || echo 'lo0 not skipped')"

# 5. Report.
say "users: $(for u in $LANES; do id "$u"; done | tr '\n' ';')"
say "staff member check: $(dseditgroup -o checkmember -m agmx1 staff 2>&1 || true)"
stat -f '%N %Lp %Su:%Sg' /Users/neo /Users/neo_1 /opt/homebrew /opt/homebrew/bin /opt/agms /opt/agms/pkg /opt/agms/pkg/bin/corbanu
mount | grep -ci virtiofs || true
cat "$S/opt-digests.txt"
say done
