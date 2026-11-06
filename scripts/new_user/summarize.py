#!/usr/bin/env python3
"""Summarises finished new-user runs: the result, the action count, and the
agent's own notes, so findings can be read without opening every log.

    scripts/new_user/summarize.py /tmp/noodle-run-S01-0 /tmp/noodle-run-S02-0
"""

import json
import sys
from pathlib import Path


def main() -> None:
    for arg in sys.argv[1:]:
        private = Path(arg) / "private"
        result = json.loads((private / "result.json").read_text()) if (private / "result.json").exists() else {}
        print(f"== {arg}")
        print(
            f"   {result.get('story', '?')}: {result.get('ended', 'not ended')}, "
            f"{'PASSED' if result.get('passed') else 'not passed'}, "
            f"{result.get('actions', '?')}/{result.get('budget', '?')} actions, "
            f"{result.get('seconds', '?')} s"
        )
        if result.get("why_not"):
            print(f"   done check: {result['why_not']}")
        log = private / "log.jsonl"
        if not log.exists():
            continue
        for line in log.read_text().splitlines():
            entry = json.loads(line)
            if entry["command"].startswith("note "):
                print(f"   [{entry['action']}] {entry['command'][5:]}")


if __name__ == "__main__":
    main()
