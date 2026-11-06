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

## All stories, zero docs, one run each

Haiku, medium effort, 80 actions, run from the same build. "Check" is the
route's done check, which for some stories is weak (see below). Per-story
notes and raw logs are kept outside the repo; findings are being reviewed
before they become issues.

| Story | Agent ended | Actions | Check | Check says |
|---|---|---|---|---|
| S01 comp | gave up | 40 | no | takes [4], wanted [3, 6] |
| S02 punch | gave up | 54 | yes | punch set (found in the ruler menu) |
| S03 external synth | gave up | 18 | no | MIDI In not wired to MIDI Out (no MIDI in the cloud) |
| S04 tighten drums | gave up | 36 | yes | transients pinned (the story is partial) |
| S05 conform | gave up | 31 | no | key and warp mode |
| S06 stems | gave up | 23 | yes | **vacuous**: only that the download question is not open |
| S07 sections | gave up | 67 | no | one marker of the wanted set |
| S08 MIDI detail | gave up | 30 | no | wanted notes [60, 64, 67, 72] |
| S09 drum pattern | gave up | 72 | no | nothing wired into Swing |
| S10 articulations | gave up | 42 | no | 0 articulations, wanted 2 |
| S11 audio to MIDI | finished | 11 | yes | |
| S12 multisampler | finished | 16 | yes | (run 4) |
| S13 macro patch | gave up | 52 | no | no Mix node |
| S14 shared modulation | gave up | 42 | no | no Sample & Hold node (agent used an LFO set to Random) |
| S15 rhythmic gate | finished | 75 | no | Saw not wired to the VCA the route uses |
| S16 spectral cross | finished | 57 | no | Synthesize not wired to Output |
| S17 resample, slice | gave up | 46 | no | no Distortion |
| S18 plugins | gave up | 30 | yes | **vacuous**: the check is `Ok(())` |
| S19 drum bus | gave up | 69 | no | no Gain after the drum bus |
| S20 automation | gave up | 39 | no | lane has 1 point |
| S21 mix versions | gave up | 18 | no | cutoff not varied per version (#387 feature not found) |
| S22 surround | gave up | 61 | no | no layout converter |
| S23 loudness | gave up | 39 | no | no Dynamics node |
| S24 stems export | gave up | 25 | yes | **weak**: passes with nothing built |
| S27 clip launching | finished | 55 | yes | |
| S28 setlist | gave up | 33 | yes | **vacuous** (the story is blocked) |
| S29 organise | finished | 42 | no | not one folded folder of two tracks |
| S30 freeze | gave up | 34 | no | reverb not frozen (Freeze exists; not found) |

Passes that mean something: S11, S12, S27. Several "no" rows are a different
valid method (S14, S16, S19, S22, S23): a route's done check is stricter than
the story. Fixing the vacuous checks and loosening the method-specific ones
is its own piece of work.
