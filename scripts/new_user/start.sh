#!/usr/bin/env bash
# Starts the Noodle session for this run and waits until it is up.
# Usage: start.sh   (run from anywhere; paths are filled in by build_run.py)
set -eu
dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="@REPO@"
story="@STORY@"
budget="@BUDGET@"
rm -rf "$dir"/{world,screens,internal,cmd.*,resp.*,ready,result.json,log.jsonl,.seq}
cd "$repo"
cargo test -p noodle-app --no-run 2>&1 | tail -2
NOODLE_AGENT_STORY="$story" NOODLE_AGENT_DIR="$dir" NOODLE_AGENT_BUDGET="$budget" \
  nohup cargo test -p noodle-app stories::agent::drive -- --ignored --nocapture \
  > "$dir/driver.log" 2>&1 &
echo $! > "$dir/driver.pid"
for _ in $(seq 1 600); do
  [ -f "$dir/ready" ] && { echo "Noodle is up. First screen: $dir/screens/001.png"; exit 0; }
  kill -0 "$(cat "$dir/driver.pid")" 2>/dev/null || { echo "the driver stopped:"; tail -30 "$dir/driver.log"; exit 1; }
  sleep 0.5
done
echo "timed out waiting for Noodle"; exit 1
