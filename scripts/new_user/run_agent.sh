#!/usr/bin/env bash
# Runs one built "new user" run with any agent that can run shell commands.
#
#   scripts/new_user/run_agent.sh <run folder> <runner> [model]
#
# It starts the app, runs the runner from a fresh empty directory with the
# prompt, stops the app, then audits the transcript if the runner wrote one.
# A runner is a script in runners/ (or a path to one) that gets, as env vars:
#   NOODLE_PROMPT      the prompt file (private/prompt.md): give it as the whole prompt
#   NOODLE_AGENT       the agent folder: ctl.sh and screens/ live here
#   NOODLE_CTL         the one command the agent may run: bash $NOODLE_CTL <command>
#   NOODLE_MODEL       the model name, if given
#   NOODLE_TRANSCRIPT  where to write the transcript (stream-json if you want audit.py)
# and runs in an empty working directory. It should run the agent to the end.
#
# Runners: runners/claude.sh (Claude Code, contained: only ctl.sh and the
# screenshots allowed), runners/command.sh (any command you give in
# NOODLE_AGENT_CMD, prompt on stdin). Write your own for another tool; see
# docs/usability/new-user-runs.md.
set -eu
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$(cd "${1:?usage: run_agent.sh <run folder> <runner> [model]}" && pwd)"
runner="${2:?name a runner: claude, command, or a path}"
[ -f "$runner" ] || runner="$here/runners/$runner.sh"
[ -f "$runner" ] || { echo "no runner $2 (looked in $here/runners)"; exit 1; }

bash "$out/private/start.sh"
work="$(mktemp -d)"   # empty, and not a checkout
cd "$work"
export NOODLE_PROMPT="$out/private/prompt.md" NOODLE_AGENT="$out/agent" \
  NOODLE_CTL="$out/agent/ctl.sh" NOODLE_MODEL="${3:-}" \
  NOODLE_TRANSCRIPT="$out/private/transcript.jsonl"
bash "$runner" || echo "the runner exited with an error (see above)"
# An agent that stopped without finish or giveup still gets its run checked.
[ -f "$out/agent/over" ] || bash "$NOODLE_CTL" abandon >/dev/null 2>&1 || true
# The app writes the result when the agent finishes; give it time before stopping it.
for _ in $(seq 1 100); do [ -f "$out/private/result.json" ] && break; sleep 0.3; done
kill "$(cat "$out/private/driver.pid")" 2>/dev/null || true
if [ -s "$NOODLE_TRANSCRIPT" ]; then
  python3 "$here/audit.py" "$out" || true
else
  echo "no transcript: not audited. Check by hand that the agent used only ctl.sh."
fi
python3 "$here/summarize.py" "$out" 2>/dev/null || true
