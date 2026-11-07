export AGMS_RUN=$HOME/runs/@RUN@ AGMS_PROXY=http://127.0.0.1:18443 PATH=/opt/agms/bin:/usr/bin:/bin
cd $AGMS_RUN/work
echo "### 1. conversation on work in the TUI (--account work), then quit"; date -u +%T
agms-tui --window c1 start --home zai3 --account work > /dev/null; agms-tui --window c1 wait glm-5.3-flash 60 > /dev/null
agms-tui --window c1 text "reply with the single word qzero" > /dev/null; sleep 1; agms-tui --window c1 key Enter > /dev/null; agms-tui --window c1 wait "• qzero" 90 > /dev/null; date -u +%T
agms-tui --window c1 text /quit > /dev/null; sleep 1; agms-tui --window c1 key Enter > /dev/null; sleep 3
agms-tui --window c1 screen c1-end | grep -E "resume|exited" | head -3
echo "### 2. account remove zai work"; date -u +%T
agms-product zai3 account remove zai work 2>&1
echo "### 3. TUI WITH --account personal ([provider_accounts] zai = "personal"); in-app /resume of the qzero conversation"; date -u +%T
agms-tui start --home zai3 --account personal > /dev/null; agms-tui wait glm-5.3-flash 60 > /dev/null
agms-tui text /resume > /dev/null; sleep 1; agms-tui key Enter > /dev/null; sleep 3
agms-tui text qzero > /dev/null; sleep 2; agms-tui key Enter > /dev/null; sleep 6
agms-tui screen p3-after-resume | grep -v '^\s*$' | tail -12
date -u +%T; agms-tui text "reply with the single word qone" > /dev/null; sleep 1; agms-tui key Enter > /dev/null; sleep 40; date -u +%T
agms-tui screen p3-after-qone | grep -v '^\s*$' | tail -8
agms-tui text /status > /dev/null; sleep 1; agms-tui key Enter > /dev/null; sleep 3
agms-tui screen p3-status | grep -E 'Model provider|Session|Account usage' | head -6
agms-tui text /quit > /dev/null; sleep 1; agms-tui key Enter > /dev/null; sleep 2; agms-tui stop > /dev/null
echo "### end"; date -u +%T
