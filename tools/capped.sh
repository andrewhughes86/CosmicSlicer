#!/usr/bin/env bash
# Runs a command (usually cargo) in its own memory-capped systemd scope, so running out of
# memory kills that command and not the editor it was started from: a test that ran away to
# 3.8 GB took VS Code's whole scope down with it on 2026-11-02 (Linux with systemd only).
#
#   tools/capped.sh cargo test -p CosmicSlicer-import -j 1
#   CAP=3G tools/capped.sh cargo clippy --workspace --all-targets -j 1 -- -D warnings
#
# Logs go to target/diag/<time>-*.log: the command's output, and every 2 s the scope's memory
# (current and peak), free memory and the biggest processes. After an out-of-memory kill the
# kernel's OOM lines are added, and any fuzz case that was running is named (fuzz.rs leaves
# its input in <tmp>/cosmicslicer-fuzz/ until the case returns).
set -u
cap="${CAP:-2G}"
root="$(cd "$(dirname "$0")/.." && pwd)"
diag="$root/target/diag"
mkdir -p "$diag"
stamp="$(date +%Y%m%d-%H%M%S)"
out="$diag/$stamp-output.log"
mem="$diag/$stamp-memory.log"
unit="cosmicslicer-capped-$stamp-$$"
since="$(date '+%Y-%m-%d %H:%M:%S')"

echo "$ $* (cap $cap)" > "$out"
systemd-run --user --scope --quiet --unit="$unit" -p MemoryMax="$cap" -p MemorySwapMax=256M \
    nice -n 19 "$@" >> "$out" 2>&1 &
pid=$!

(
    cg=""
    while kill -0 "$pid" 2>/dev/null; do
        if [ -z "$cg" ]; then
            cg="$(systemctl --user show -p ControlGroup --value "$unit.scope" 2>/dev/null)"
        fi
        {
            echo "== $(date +%T)"
            now="$(cat "/sys/fs/cgroup$cg/memory.current" 2>/dev/null)"
            top="$(cat "/sys/fs/cgroup$cg/memory.peak" 2>/dev/null)"
            if [ -n "$cg" ] && [ -n "$now" ]; then
                echo "scope MB: current $((now >> 20)), peak $((${top:-0} >> 20))"
            fi
            free -m | sed -n 2,3p
            ps -eo rss=,comm= --sort=-rss | head -5 | awk '{printf "  %6d MB %s\n", $1/1024, $2}'
        } >> "$mem"
        sleep 2
    done
) &
sampler=$!

wait "$pid"
status=$?
wait "$sampler" 2>/dev/null

peak="$(grep -o 'peak [0-9]*' "$mem" 2>/dev/null | tail -1)"
echo "exit $status, scope ${peak:-peak unknown} MB" >> "$out"
if [ "$status" -eq 137 ] || [ "$status" -eq 143 ] || journalctl --user --since "$since" --no-pager 2>/dev/null | grep -q "$unit.*oom"; then
    {
        echo "== killed: out of memory? kernel says:"
        journalctl -k --since "$since" --no-pager 2>/dev/null | grep -i -E 'out of memory|oom-kill' | tail -5
        echo "== fuzz cases that never returned:"
        ls -l "${TMPDIR:-/tmp}/cosmicslicer-fuzz/" 2>/dev/null
    } >> "$out"
fi
echo "exit $status; ${peak:-peak unknown} MB; log $out; memory $mem"
exit "$status"
