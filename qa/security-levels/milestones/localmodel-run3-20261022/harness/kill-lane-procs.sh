echo PF83BEGIN
ps -axo pid,uid,comm | awk '$2>=610 && $2<=613'
for p in $(ps -axo pid=,uid= | awk '$2>=610 && $2<=613 {print $1}'); do kill -9 $p 2>/dev/null; done
sleep 2
echo "[kill-lane-procs] processes for removed uids: $(ps -axo uid= | awk '$1>=610 && $1<=613' | wc -l)"
