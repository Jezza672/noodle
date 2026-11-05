# New-user agent runs

Findability, measured by an agent that has never seen Noodle (metric E, "Naive
success", ticket U13 / #319). The agent gets the story's own words and the
running app, nothing from `docs/`. Never run per PR: each run costs model time.

## What an agent gets

- The story text from [stories.md](stories.md), without its late changes, and
  where its files are.
- One helper, `ctl.sh`, to look (screenshot, accessibility tree) and act
  (click, right click, double click, drag, hover, scroll, type, key). Its
  commands are the same input events `testkit` gives a route.
- An action budget (80 by default). Looking and note-taking are free.
- In a clue run, the clues so far, worded as a friend who uses Noodle would.

It is told to think aloud with `note`, and not to read any other file. That
rule is only an instruction: check the transcript, not just the log.

## Making a run

```sh
python3 scripts/new_user/build_run.py --story S12 --out /tmp/noodle-run-s12-0            # zero docs
python3 scripts/new_user/build_run.py --story S12 --clues 2 --out /tmp/noodle-run-s12-2  # two clues
bash /tmp/noodle-run-s12-0/private/start.sh   # builds and starts the app (software Vulkan is fine)
```

The run folder has two parts. `agent/` is all the agent is pointed at: `ctl.sh`,
then the screens and its files. `private/` is not: `prompt.md`, `start.sh`
(which names the repo), `run.json`, and the results. Use a neutral `--out`
(no repo path in it), and run the agent from an empty directory, because the
prompt only *asks* it not to read other files. A Claude subagent inherits the
repo as its working directory, so check its transcript for stray reads.

Give `private/prompt.md` to an agent with a shell as its whole prompt. Any
model works: the driver is plain files, no SDK. The build is the one the repo
is at, recorded in `run.json`. On Linux without a GPU, install
`mesa-vulkan-drivers` and `libasound2-dev`.

When the agent says `finish` or `giveup`, or runs out of actions, `private/`
holds:

| File | What |
|---|---|
| `result.json` | Whether the story's done check passed (the agent is never told), actions used, seconds |
| `log.jsonl` | Each command with its result, the widget and view it hit, the agent's notes |
| `run.json` | Story, clues given, budget, build |

and `agent/screens/` has the screen after every action. The prompts are not
kept in the repo: rebuild one with the same `--story` and `--clues`.

The driver is `crates/noodle-app/src/stories/agent.rs`, an ignored test. Its
start state is the route's fixture, so the done check is the route's.

## Clues

A run with zero docs comes first. When it fails or stalls, read its notes and
screens, then add **one** clue to `new-user/clues.json` under the story ID:
what a friend would say that unblocks the stuck step, no more. Each entry
records the clue and why it was needed (the step the previous run stuck on).
The next run is a fresh agent with all clues so far
(`--clues N`), so runs are comparable and a clue's effect is visible. Stop
when the story passes. The clues are the findability findings: each is a
missing signpost, and a candidate issue.

`new-user/tasks.json` says where each story's files are. Results are in
[new-user/results.md](new-user/results.md).
