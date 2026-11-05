# Using a music program for the first time

You are trying a music program called **Noodle** for the first time. You have
never used it, you have no manual and nobody to ask. Work out how to do the
task below by looking at the screen and trying things, the way a person would.

## The task

Build a multisampled instrument from a folder. As a sound designer, I want to turn a folder of recordings, each named after a note, into a playable instrument using samplers, so that any input note plays the closest recorded note, pitch-shifted by the difference.

Your files for this are in the folder `/tmp/claude-0/-home-user-noodle/ee7cad98-328b-5ed2-bc9d-a688e8825c22/scratchpad/s12-1/world/recordings`.

## How you use the program

You can only see and use Noodle through one helper script. Every command is
`bash /tmp/claude-0/-home-user-noodle/ee7cad98-328b-5ed2-bc9d-a688e8825c22/scratchpad/s12-1/ctl.sh <command>`, and prints what happened.

| Command | What it does |
|---|---|
| `look` | Takes a screenshot of the window and prints where the file is (`screenshot: .../NNN.png`). Open that file with your image viewer or file-reading tool to see it. |
| `tree` | Lists the text and rectangles the window exposes to screen readers: `Role "label" [x1,y1 to x2,y2]`. Handy for finding exact positions. |
| `click X Y [shift\|ctrl\|alt]` | Left click at pixel X, Y of the screenshot. |
| `rclick X Y` | Right click. |
| `dclick X Y` | Double click. |
| `drag X1 Y1 X2 Y2 [more X Y pairs] [shift\|ctrl\|alt]` | Press at the first point, move through the rest, release at the last. |
| `hover X Y` | Move the pointer without clicking (tooltips). |
| `scroll X Y DX DY` | Mouse wheel at X, Y. Negative DY scrolls down. |
| `type some text` | Types the text into whatever has keyboard focus (click a field first). |
| `key NAME` | One key press, with the pointer where you last put it: `key Enter`, `key Escape`, `key Delete`, `key Space`, `key ctrl+z`, `key shift+a`. |
| `choose /full/path` | Answers a file picker that has opened (the program's reply tells you when one has). Several files: separate paths with `;`. A folder picker takes a folder. |
| `cancel` | Closes a file picker without choosing. |
| `note your thoughts` | Writes down what you are thinking. Free, and does not touch the program. |
| `finish` | Say you believe the task is done. This ends the session. |
| `giveup` | Say you cannot do it. This ends the session. |

The window is 1280 by 800 pixels, and every action after the first one prints
the path of a fresh screenshot. The first screenshot is `/tmp/claude-0/-home-user-noodle/ee7cad98-328b-5ed2-bc9d-a688e8825c22/scratchpad/s12-1/screens/001.png`.
The pointer starts in the middle of the window.

You have **80 actions** (clicks, drags, typing, keys, scrolls and
hovers). `look`, `tree` and `note` are free. When they run out the session is
over.

## Rules

- Think aloud: before every action, run `note` with what you are trying and
  why, and when something surprises you, note that too. Your notes are how the
  people who made the program learn where it confuses people. Be honest about
  being lost.
- Use only `/tmp/claude-0/-home-user-noodle/ee7cad98-328b-5ed2-bc9d-a688e8825c22/scratchpad/s12-1/ctl.sh`, and open only the screenshot files it prints.
  Do **not** read any other file or folder on this computer except the files
  named in the task, and do not search the web: nobody sitting at the program
  would have the program's source or its manual.
- Do not guess your way to a "finished" state: say `finish` only when you
  believe the task is really done, and `giveup` when you are truly stuck.

## When the session is over

Reply with a short report: did you get the task done, what was the hardest
moment, and what do you wish the program had shown you.
