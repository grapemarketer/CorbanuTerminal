# Guest environment manifest (review r1), run as the image admin over SSH: tool versions, console state, hardware.
echo "python3: $(/usr/bin/python3 --version 2>&1)"
echo "git: $(/usr/bin/git --version 2>&1)"
echo "bash: $(/bin/bash --version | head -1)"
echo "who: $(who | tr '\n' ';')"
echo "loginwindow: $(pgrep -x loginwindow | wc -l | tr -d ' ') process(es)"
echo "SecurityAgent: $(pgrep -x SecurityAgent | wc -l | tr -d ' ') process(es)"
echo "hw: $(sysctl -n hw.ncpu) CPUs, $(sysctl -n hw.memsize) bytes, hv_vmm_present=$(sysctl -n kern.hv_vmm_present 2>/dev/null)"
