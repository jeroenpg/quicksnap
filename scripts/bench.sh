#!/usr/bin/env bash
# Measure how long each tool takes before you can start selecting.
# QuickSnap: trigger -> overlay visible (from its own log), N runs.
# Spectacle / Flameshot: time until their capture is complete. Their region overlay can only
# appear after that, so these numbers are a lower bound for "ready to select".
# The QuickSnap runs briefly dim your screens.
set -uo pipefail
N=${1:-15}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

median() { sort -n | awk '{a[NR]=$1} END {print (NR%2 ? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2)}'; }
ms() { local s=$(date +%s%N); "$@" >/dev/null 2>&1; echo $(( ($(date +%s%N) - s) / 1000000 )); }

echo "Screens: $(kscreen-doctor -o 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | grep -c '^\s*enabled')"

if systemctl --user is-active -q quicksnap; then
    since=$(date '+%Y-%m-%d %H:%M:%S')
    for _ in $(seq "$N"); do
        quicksnap capture; sleep 0.9; quicksnap cancel; sleep 0.6
    done
    journalctl --user -u quicksnap --since "$since" --no-pager -o cat \
        | grep -oP 'overlay visible \K[0-9]+' > "$tmp/qs"
    echo "QuickSnap  overlay visible: median $(median < "$tmp/qs") ms  (runs: $(tr '\n' ' ' < "$tmp/qs"))"
fi

if command -v spectacle >/dev/null; then
    for _ in $(seq 5); do ms spectacle -b -n -m -o "$tmp/s.png"; done > "$tmp/sp"
    echo "Spectacle  capture current screen: median $(median < "$tmp/sp") ms"
fi

if command -v flameshot >/dev/null; then
    for _ in $(seq 3); do ms timeout 60 sh -c "flameshot full --raw > '$tmp/f.png'"; done > "$tmp/fl"
    echo "Flameshot  capture all screens: median $(median < "$tmp/fl") ms"
fi
