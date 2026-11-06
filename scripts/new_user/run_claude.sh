#!/usr/bin/env bash
# Runs one built "new user" run with Claude Code, contained: from an empty
# directory, with only the tools the agent needs, and only the one command and
# the screenshots allowed. Anything else is denied (not just asked not to).
#
#   scripts/new_user/run_claude.sh /tmp/noodle-run-s12-0 [model]   # default: haiku
#
# Then audits the transcript (audit.py). For another model or tool, give it
# private/prompt.md yourself and run audit.py on whatever transcript you get.
set -eu
out="$(cd "${1:?usage: run_claude.sh <run folder> [model]}" && pwd)"
model="${2:-haiku}"
bash "$out/private/start.sh"
work="$(mktemp -d)"   # empty, and not a checkout
cd "$work"
claude -p "$(cat "$out/private/prompt.md")" \
  --model "$model" --effort medium \
  --tools "Bash,Read" \
  --allowedTools "Bash(bash $out/agent/ctl.sh:*)" "Read($out/agent/screens/**)" \
  --permission-mode dontAsk \
  --setting-sources "" --strict-mcp-config --disable-slash-commands \
  --output-format stream-json --verbose \
  > "$out/private/transcript.jsonl" 2> "$out/private/claude.err" || true
kill "$(cat "$out/private/driver.pid")" 2>/dev/null || true
python3 "$(dirname "${BASH_SOURCE[0]}")/audit.py" "$out" || true
python3 "$(dirname "${BASH_SOURCE[0]}")/summarize.py" "$out" 2>/dev/null || true
