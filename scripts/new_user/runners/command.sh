#!/usr/bin/env bash
# Runs any agent command you give in NOODLE_AGENT_CMD, with the prompt on its
# standard input, from the empty directory. Nothing is contained: the agent is
# only asked to stay inside ctl.sh. The command may use $NOODLE_PROMPT,
# $NOODLE_CTL, $NOODLE_AGENT and $NOODLE_MODEL.
#
#   NOODLE_AGENT_CMD='my-agent --model "$NOODLE_MODEL" --yes' \
#     scripts/new_user/run_agent.sh /tmp/noodle-run-s12-0 command some-model
#
# Its output is kept as the transcript. audit.py only understands Claude Code's
# stream-json, so for other output the run is reported as not audited.
set -u
: "${NOODLE_AGENT_CMD:?set NOODLE_AGENT_CMD to the command that runs your agent}"
bash -c "$NOODLE_AGENT_CMD" < "$NOODLE_PROMPT" > "$NOODLE_TRANSCRIPT.txt" 2>&1
cat "$NOODLE_TRANSCRIPT.txt" | tail -40
