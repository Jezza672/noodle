# New-user run results

Protocol: [../new-user-runs.md](../new-user-runs.md). Agent: Haiku, medium
effort, zero docs unless a clue is listed. Budget 80 actions. Build:
`main` after #387 and #389. Raw logs (every command, the widget it hit, the
agent's notes) are in `runs/`.

## S12, build a multisampled instrument from a folder

| Run | Clues | Ended | Actions | Done check | Notes |
|---|---|---|---|---|---|
| 0 | 0 | gave up | 26 | n/a | **Void.** The harness had no file picker, so no agent could load a file. Fixed by `picker.rs`. It still found Shift+A and the Sample Set. |
| 1 | 0 | finished | 21 | failed | The agent built the right graph. The check wanted 4 recordings (the scorecard route sets the rest aside), the folder held 8. Fixed by asking the check about the fixture's size. Counted as right in substance, not as a pass. |
| 2 | 0 | gave up | 31 | failed | Never found the Sample Set. Searched "range" (nothing), then built key zones by hand with a Note Split. Gave up believing the budget was nearly spent (49 actions were left). |
| 3 | 1 | finished | 22 | **passed** | With clue 1. |
| 4 | 0 | finished | 16 | **passed** | Found the Sample Set by searching "sample" after "multi" matched nothing. |

Zero docs: one pass, one clear failure, one pass in substance. Same model,
same prompt, so the findability of the Sample Set is borderline: it depends on
which word the agent happens to search for.

### Clues, in order

Recorded in [clues.json](clues.json), with why each was needed.

1. *Noodle has a node made for loading a whole folder of recordings at once,
   and it reads each note from the file names. It is among the instruments in
   the Add menu (Shift+A over the node canvas).* Needed because run 2 never
   found the Sample Set. With it, the story passed in 22 actions.

Only one clue was needed. A second zero-docs failure would give the next one.

### What the agents got lost on

Every run, in the agents' own notes, unprompted:

- **Adding a node.** No button, right click does nothing. All four runs
  guessed Shift+A from the Blender look. Nothing on screen says it.
- **"No sample loaded".** The Sampler's panel says this while a Sample Set
  feeds it. Runs 1, 3 and 4 could not tell whether it worked, and all three
  finished without being sure. It reads like a failure.
- **Sample Set needs a Sampler.** Its output tooltip says only "Sample set".
  Nothing says a Sampler plays it. The Sample Set's own name suggests it is
  the instrument.
- **Search words.** "multi" and "range" match nothing; "sample" finds the
  Sample Set. "key" finds a keyboard input, not a zone.
- **Number fields.** A click opens the field with the old value in it, so
  typed text is appended. It needs Ctrl+A first. Return was needed to commit
  in one case and not in another.

### Harness limits found on the way

- A native file picker cannot be seen in a screenshot, so the driver tells the
  agent one opened and takes the path with `choose`. The agent in run 2 still
  cancelled one, because it could not see it.
- Agents cannot hear. "Does it play?" was never answered by any agent, so the
  done check, which listens, is the only judge. The agents' own confidence is
  not a result.

### Next

User-facing docs, then the same runs with the agent able to look them up (not
pasted into its prompt). More stories: the other `full` ones whose fixture
needs a file or two (S09, S23).
