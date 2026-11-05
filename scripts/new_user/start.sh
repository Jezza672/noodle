#!/usr/bin/env bash
# Starts the Noodle session for this run and waits until it is up.
# Usage: start.sh   (run from anywhere; paths are filled in by build_run.py).
# Lives in private/, which the agent is never pointed at.
set -eu
private="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dir="@AGENT@"
repo="@REPO@"
story="@STORY@"
budget="@BUDGET@"
size="@SIZE@"   # empty: the route's own size
rm -rf "$dir"/{world,screens,cmd.*,resp.*,ready,over,.seq} "$private"/{result.json,log.jsonl,prefs.ron,driver.log,driver.pid}
cd "$repo"
cargo test -p noodle-app --no-run 2>&1 | tail -2
env NOODLE_AGENT_STORY="$story" NOODLE_AGENT_DIR="$dir" NOODLE_AGENT_PRIVATE="$private" NOODLE_AGENT_BUDGET="$budget" ${size:+NOODLE_AGENT_SIZE=$size} \
  nohup cargo test -p noodle-app stories::agent::drive -- --ignored --nocapture \
  > "$private/driver.log" 2>&1 &
echo $! > "$private/driver.pid"
for _ in $(seq 1 600); do
  [ -f "$dir/ready" ] && { echo "Noodle is up. First screen: $dir/screens/001.png"; exit 0; }
  kill -0 "$(cat "$private/driver.pid")" 2>/dev/null || { echo "the driver stopped:"; tail -30 "$private/driver.log"; exit 1; }
  sleep 0.5
done
echo "timed out waiting for Noodle"; exit 1
