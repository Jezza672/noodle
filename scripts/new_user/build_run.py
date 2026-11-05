#!/usr/bin/env python3
"""Builds a "new user" run: the prompt for an agent that has never seen
Noodle, and everything it needs to drive the app.

    scripts/new_user/build_run.py --story S12 --out /tmp/run-s12-0
    scripts/new_user/build_run.py --story S12 --clues 2 --out /tmp/run-s12-2

The output folder holds prompt.md (give it to any model that has a shell),
ctl.sh (the agent's hands), start.sh (starts the app), run.json and a README.
No docs from the repo are included: the agent gets the story's own words, how
to press buttons through ctl.sh, and (with --clues N) the first N clues, which
are what a friend who already uses Noodle might say. Clues live in
docs/usability/new-user/clues.json; see docs/usability/new-user-runs.md.
"""

import argparse
import datetime
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
STORIES = REPO / "docs/usability/stories.md"
NEW_USER = REPO / "docs/usability/new-user"

PROMPT = """\
# Using a music program for the first time

You are trying a music program called **Noodle** for the first time. You have
never used it, you have no manual and nobody to ask. Work out how to do the
task below by looking at the screen and trying things, the way a person would.

## The task

{story}

{files}

## How you use the program

You can only see and use Noodle through one helper script. Every command is
`bash {ctl} <command>`, and prints what happened.

| Command | What it does |
|---|---|
| `look` | Takes a screenshot of the window and prints where the file is (`screenshot: .../NNN.png`). Open that file with your image viewer or file-reading tool to see it. |
| `tree` | Lists the text and rectangles the window exposes to screen readers: `Role "label" [x1,y1 to x2,y2]`. Handy for finding exact positions. |
| `click X Y [shift\\|ctrl\\|alt]` | Left click at pixel X, Y of the screenshot. |
| `rclick X Y` | Right click. |
| `dclick X Y` | Double click. |
| `drag X1 Y1 X2 Y2 [more X Y pairs] [shift\\|ctrl\\|alt]` | Press at the first point, move through the rest, release at the last. |
| `hover X Y` | Move the pointer without clicking (tooltips). |
| `scroll X Y DX DY` | Mouse wheel at X, Y. Negative DY scrolls down. |
| `type some text` | Types the text into whatever has keyboard focus (click a field first). |
| `key NAME` | One key press, with the pointer where you last put it: `key Enter`, `key Escape`, `key Delete`, `key Space`, `key ctrl+z`, `key shift+a`. |
| `choose /full/path` | Answers a file picker that has opened (the program's reply tells you when one has). Pickers are native dialogs that screenshots do not show, so you type the path you want. Several files: separate paths with `;`. A folder picker takes a folder. |
| `cancel` | Closes a file picker without choosing. |
| `note your thoughts` | Writes down what you are thinking. Free, and does not touch the program. |
| `finish` | Say you believe the task is done. This ends the session. |
| `giveup` | Say you cannot do it. This ends the session. |

The window is 1280 by 800 pixels, and every action after the first one prints
the path of a fresh screenshot. The first screenshot is `{first}`.
The pointer starts in the middle of the window.

You have **{budget} actions** (clicks, drags, typing, keys, scrolls and
hovers). `look`, `tree` and `note` are free. When they run out the session is
over.

## Rules

- Think aloud: before every action, run `note` with what you are trying and
  why, and when something surprises you, note that too. Your notes are how the
  people who made the program learn where it confuses people. Be honest about
  being lost.
- Use only `{ctl}`, and open only the screenshot files it prints.
  Do **not** read any other file or folder on this computer except the files
  named in the task, and do not search the web: nobody sitting at the program
  would have the program's source or its manual.
- Do not guess your way to a "finished" state: say `finish` only when you
  believe the task is really done, and `giveup` when you are truly stuck.
{clues}
## When the session is over

Reply with a short report: did you get the task done, what was the hardest
moment, and what do you wish the program had shown you.
"""

CLUES = """\

## Hints from a friend who already uses Noodle

You asked a friend. This is everything they told you:

{items}
"""


def story_text(story: str) -> str:
    """The story's own words, without its late changes."""
    lines = STORIES.read_text().splitlines()
    for i, line in enumerate(lines):
        if line.startswith(f"- **{story}** "):
            body = re.sub(r"^- \*\*S\d+\*\* ", "", line)
            return body.replace("**", "").strip()
    sys.exit(f"{story} is not in {STORIES}")


def load_json(path: Path, default):
    return json.loads(path.read_text()) if path.exists() else default


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--story", required=True, help="a story ID, such as S12")
    ap.add_argument("--clues", type=int, default=0, help="how many clues to include")
    ap.add_argument("--budget", type=int, default=80, help="actions the agent gets")
    ap.add_argument("--out", required=True, help="folder to build the run in")
    args = ap.parse_args()

    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    clues_all = load_json(NEW_USER / "clues.json", {}).get(args.story, [])
    if args.clues > len(clues_all):
        sys.exit(f"{args.story} has only {len(clues_all)} clues recorded")
    clues = clues_all[: args.clues]
    tasks = load_json(NEW_USER / "tasks.json", {}).get(args.story, {})

    files = tasks.get("files")
    where = f"{out}/world" + (f"/{files}" if files else "")
    files_text = (
        f"Your files for this are in the folder `{where}`."
        if files
        else f"Any files you need are in the folder `{where}`."
    )
    items = "\n".join(f"{i}. {c['clue']}" for i, c in enumerate(clues, 1))
    prompt = PROMPT.format(
        story=story_text(args.story),
        files=files_text,
        ctl=f"{out}/ctl.sh",
        first=f"{out}/screens/001.png",
        budget=args.budget,
        clues=CLUES.format(items=items) if clues else "",
    )
    (out / "prompt.md").write_text(prompt)

    size = tasks.get("size")
    for name in ("ctl.sh", "start.sh"):
        text = (HERE / name).read_text()
        text = (
            text.replace("@REPO@", str(REPO))
            .replace("@STORY@", args.story)
            .replace("@BUDGET@", str(args.budget))
            .replace("@SIZE@", str(size) if size else "")
        )
        (out / name).write_text(text)
        (out / name).chmod(0o755)

    commit = subprocess.run(
        ["git", "-C", str(REPO), "rev-parse", "--short", "HEAD"],
        capture_output=True,
        text=True,
    ).stdout.strip()
    (out / "run.json").write_text(
        json.dumps(
            {
                "story": args.story,
                "clues_given": [c["clue"] for c in clues],
                "budget": args.budget,
                "build": commit,
                "built": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
            },
            indent=2,
        )
        + "\n"
    )
    (out / "README.md").write_text(
        f"""# New-user run: {args.story}, {len(clues)} clue(s)

1. `bash {out}/start.sh` builds and starts Noodle and waits until it is up.
2. Give the agent `{out}/prompt.md` as its whole prompt (it needs a shell, and
   a way to look at PNG files if it can see images). Give it nothing else.
3. When the agent ends (it says finish or giveup, or runs out of actions),
   `{out}/result.json` holds whether the story's done check passed.
   `{out}/log.jsonl` has every command, what it hit, and the agent's notes.
4. Stop the app if it is still running: `kill $(cat {out}/driver.pid)`.

Protocol and how to record clues: `docs/usability/new-user-runs.md`.
"""
    )
    print(out)


if __name__ == "__main__":
    main()
