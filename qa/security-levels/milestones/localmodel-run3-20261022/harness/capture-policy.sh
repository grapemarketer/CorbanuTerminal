echo PF83BEGIN
echo "## captured $(date -u +%FT%TZ) as $(id -un)"
echo "## pfctl -sI -v (lo0 skip state)"; pfctl -sI -v 2>/dev/null | grep -E "lo0|ALL" 
echo "## pfctl -a corbanu.pf83 -vsr"; pfctl -a corbanu.pf83 -vsr 2>/dev/null
echo "## /etc/sudoers.d/agms-codeblind"; cat /etc/sudoers.d/agms-codeblind
echo "## accounts"; for u in agmx0 agmx1 agmx2 agmx3; do id $u; dscl . -read /Users/$u UserShell IsHidden NFSHomeDirectory 2>/dev/null | tr '\n' ' '; echo; dseditgroup -o checkmember -m $u admin 2>&1; done
echo "## ssh access group"; dscl . -read /Groups/com.apple.access_ssh GroupMembership 2>&1 | head -2
echo "## modes"; stat -f '%N %Lp %Su:%Sg' /Users/* /opt/homebrew /opt/homebrew/bin /opt/homebrew/sbin /opt/agms /opt/agms/* /opt/agms/pkg/bin/* /Users/agent/.pf83-auth /Users/agent/.pf83-auth/* 2>/dev/null
echo "## /etc/codex"; ls -la /etc/codex 2>&1
echo "## virtiofs"; mount | grep -i -E "virtiofs|Shared Files" || echo "not mounted"
echo "## listeners"; lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null | awk '{print $1,$3,$9}' | sort -u
