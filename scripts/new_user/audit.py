#!/usr/bin/env python3
"""Checks that an agent stayed inside the run: it may call ctl.sh and read
screenshots, and nothing else. Reads a Claude Code stream-json transcript
(private/transcript.jsonl, or the path given) and writes private/audit.json.

    scripts/new_user/audit.py /tmp/noodle-run-s12-0 [transcript.jsonl]

A run with stray calls is "contaminated": its result says what the agent could
have found, not what a newcomer finds. Exit code 1 if contaminated. Other
runners (non-Claude) can produce a transcript in the same shape, or write their
own audit.json with {"stray": [...]}.
"""
import json
import os
import re
import sys
from pathlib import Path


def stray_calls(lines, agent: Path):
    ctl = str(agent / "ctl.sh")
    screens = str(agent / "screens") + "/"
    stray = []
    for line in lines:
        try:
            ev = json.loads(line)
        except ValueError:
            continue
        content = ev.get("message", {}).get("content", [])
        if not isinstance(content, list):
            continue
        for part in content:
            if part.get("type") != "tool_use":
                continue
            name, args = part.get("name", ""), part.get("input", {})
            if name == "Bash":
                cmd = args.get("command", "").strip()
                # exactly one ctl.sh invocation: no chaining, substitution or redirects
                ok = re.fullmatch(r"bash " + re.escape(ctl) + r" [^;&|`$<>\n]*", cmd)
                if not ok:
                    stray.append({"tool": name, "input": cmd})
            elif name == "Read":
                # Resolve first: screens/../../private/result.json starts with screens/.
                path = os.path.normpath(args.get("file_path", ""))
                if not path.startswith(screens) or not path.endswith(".png"):
                    stray.append({"tool": name, "input": path})
            else:
                stray.append({"tool": name, "input": json.dumps(args)[:200]})
    return stray


def main() -> None:
    out = Path(sys.argv[1]).resolve()
    transcript = Path(sys.argv[2]) if len(sys.argv) > 2 else out / "private/transcript.jsonl"
    if not transcript.exists():
        sys.exit(f"no transcript at {transcript}")
    stray = stray_calls(transcript.read_text().splitlines(), out / "agent")
    (out / "private/audit.json").write_text(
        json.dumps({"contaminated": bool(stray), "stray": stray}, indent=2) + "\n"
    )
    print(f"audit: {'CONTAMINATED, ' + str(len(stray)) + ' stray call(s)' if stray else 'clean'}")
    for s in stray:
        print(f"  {s['tool']}: {s['input']}")
    sys.exit(1 if stray else 0)


if __name__ == "__main__":
    main()
