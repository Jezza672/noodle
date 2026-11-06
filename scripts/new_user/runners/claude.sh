#!/usr/bin/env bash
# Claude Code runner, contained: only the Bash and Read tools, only
# `bash ctl.sh ...` and the run's screenshots allowed, everything else denied.
# Model: NOODLE_MODEL (default haiku). A Read rule starting with one slash is
# relative to the project, so the absolute path gets a second one.
set -u
claude -p "$(cat "$NOODLE_PROMPT")" \
  --model "${NOODLE_MODEL:-haiku}" --effort medium \
  --tools "Bash,Read" \
  --allowedTools "Bash(bash $NOODLE_CTL:*)" "Read(/$NOODLE_AGENT/screens/**)" \
  --permission-mode dontAsk \
  --setting-sources "" --strict-mcp-config --disable-slash-commands \
  --output-format stream-json --verbose \
  > "$NOODLE_TRANSCRIPT" 2> "$NOODLE_TRANSCRIPT.err"
