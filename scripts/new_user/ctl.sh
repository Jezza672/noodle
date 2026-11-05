#!/usr/bin/env bash
# Sends one command to the running Noodle session and prints the answer.
#   ctl.sh click 640 400
# The commands are listed in prompt.md. Safe to call from any shell.
set -u
dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ -f "$dir/ready" ] || { echo "Noodle is not running yet (no ready file in $dir)"; exit 1; }
[ $# -gt 0 ] || { echo "usage: ctl.sh <command> [arguments]"; exit 1; }
if [ -f "$dir/result.json" ]; then echo "The session is over."; exit 0; fi
seq=$(( $(cat "$dir/.seq" 2>/dev/null || echo 0) + 1 ))
echo "$seq" > "$dir/.seq"
printf '%s\n' "$*" > "$dir/cmd.$seq.tmp"
mv "$dir/cmd.$seq.tmp" "$dir/cmd.$seq"
for _ in $(seq 1 3000); do
  if [ -f "$dir/resp.$seq" ]; then cat "$dir/resp.$seq"; exit 0; fi
  sleep 0.1
done
echo "no answer from Noodle (it may have crashed)"; exit 1
