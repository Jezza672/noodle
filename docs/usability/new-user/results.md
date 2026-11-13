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

## Round 2: the same stories after the first batch of fixes

Haiku, medium effort, 80 actions, zero docs, each run **contained** (only
`ctl.sh`, `head`/`tail`/`sleep` and the screenshots allowed; see
[new-user-runs.md](../new-user-runs.md)) and audited. Built from main after
the issues from round 1 (#396-#435) were merged. Differences from round 1 to
keep in mind: the done checks were tightened (#434, #435), so some round-1
passes no longer pass; and agents can no longer `ls` their folder, so they use
`files`.

| Story | Round 1 | Round 2 | Round 2: where it ended |
|---|---|---|---|
| S01 comp | no | no | looped recording does not wrap into takes (verify) |
| S02 punch | yes | **yes** | punch found via the palette; stopped short of recording over a take |
| S03 external synth | no | no | no MIDI in the cloud; could not make a MIDI part |
| S04 tighten drums | yes (weak) | **yes** | used Quantise hits at 70% with crossfade: the new feature, found |
| S05 conform | no | no | key and formants set; playback silent, no route to output found |
| S06 stems | yes (vacuous) | no | model download fails certificate check in the cloud |
| S07 sections | no | no | arrangement edited, markers not as wanted |
| S08 MIDI detail | no | no | could not create a MIDI clip |
| S09 drum pattern | no | no | Drum Rack slot has no sample field; swing not reached |
| S10 articulations | no | no | made three articulations; could not create a MIDI clip |
| S11 audio to MIDI | yes | **yes** | |
| S12 multisampler | yes | **yes** | built the instrument; could not make a clip to test it |
| S13 macro patch | no | no | no macro found ("Make external" is on a port) |
| S14 shared modulation | no | no | found Sample & Hold; no control for per-link depth found |
| S15 rhythmic gate | no | no | out of actions, gate not chopping |
| S16 spectral cross | no | no | no node found that makes a spectrum |
| S17 resample, slice | no | no | render reports the track silent (no instrument) |
| S18 plugins | yes (vacuous) | no | no plugins in the cloud; plugin browser never opened |
| S19 drum bus | no | no | Dynamics ports too small to tell the sidechain port apart |
| S20 automation | no | no | Touch/Latch recorded nothing (verify) |
| S21 mix versions | no | no | Export button clipped out of reach when a range is set |
| S22 surround | no | no | same clipped Export button |
| S23 loudness | no | no | "nothing to export": the fixture has no clips |
| S24 stems export | yes (vacuous) | no | the fixture has no clips to export |
| S27 clip launching | yes | no | could not get a loop onto a pad |
| S28 setlist | yes (vacuous) | no | blocked (no setlist; #366) |
| S29 organise | no | no | found Ctrl+Shift+F; the described 150-track session is not in the fixture |
| S30 freeze | no | no | the described 60-track project is not in the fixture |

Meaningful passes: 4 of 28 (S02, S04, S11, S12), against 3 in round 1 (S11,
S12, S27). The four vacuous round-1 passes are now honest failures.

**Improved:** Add via right-click now works (S12 found the Add menu at once);
the punch commands are in the palette; Quantise hits at a strength is found
and finishes S04; S14 found Sample & Hold.

**Still hard:** creating a MIDI clip (S08, S10, S12, S09, S03), the macro and
per-link depth features (S13, S14), and getting sound out of a track.

**Fixed afterwards (#451):** the S23, S24, S29 and S30 fixtures now hold the story's project (clips to export, 150 tracks, 60 tracks), and their done checks fail on a smaller one, so the runs above are void and need repeating.

**Harness gaps this round exposed:** the fixtures for S23, S24, S29 and S30 do
not hold the project the story describes (no clips, 3 tracks, an empty
`project.ron`); fixing them is a scorecard issue.

## Round 3: after the double-click fix (#447)

Same setup as round 2, on `main` after #447. Rounds 1 and 2 could not make a
MIDI clip by double-click, because the agent driver's frame clock put two
clicks 0.5 s apart and egui never saw a double click. That is fixed, so this
is the first valid run of everything that depends on double-click. S09 and S27
were re-run once because the first attempt was cut short (S09 died after 11 s,
S27 lost its audit when the machine restarted); both attempts are in the raw
logs. Results are in `/mnt/project-files/new-user-runs/round3/`.

| Story | R2 | R3 | Round 3: where it ended |
|---|---|---|---|
| S01 comp | no | no | 8 takes made; no comp menu found (the hint says "right-click a take folder") |
| S02 punch | yes | no | punch found; ran bars 3 to 4 instead of 2 to 3 (likely off by one bar; check) |
| S03 external synth | no | no | no MIDI in the cloud |
| S04 tighten drums | yes | **yes** | |
| S05 conform | no | no | key set with formants kept; check wants a different warp mode |
| S06 stems | no | no | model download fails in the cloud |
| S07 sections | no | no | markers named but not on the wanted bars |
| S08 MIDI detail | no | no | made the clip by double-click; the part to edit is not in the fixture |
| S09 drum pattern | no | no | pattern built, silent (no sample reaches the rack) |
| S10 articulations | no | no | articulations set and notes tagged; check found no first note |
| S11 audio to MIDI | yes | **yes** | |
| S12 multisampler | yes | **yes** | |
| S13 macro patch | no | no | built with Version Value and Remap; no macro knob |
| S14 shared modulation | no | no | no per-link depth control |
| S15 rhythmic gate | no | no | wiring not as the check wants |
| S16 spectral cross | no | **yes** | |
| S17 resample, slice | no | no | silent: cannot tell MIDI from audio ports |
| S18 plugins | no | no | no plugins in the cloud |
| S19 drum bus | no | no | no pre-fader send, no delay-compensation control found |
| S20 automation | no | no | Touch and Latch worked; trimming a span found no way |
| S21 mix versions | no | no | cutoff not varied per version |
| S22 surround | no | no | built a 7.1.4 bed and a stereo fold-down; the progress bar looked stuck at 0-2% (a harness artefact, fixed below) |
| S23 loudness | no | no | fixture has no clips |
| S24 stems export | no | no | fixture has no clips |
| S27 clip launching | no | no | loop imported; no wire from the Sampler to the pads |
| S28 setlist | no | no | blocked (#366) |
| S29 organise | no | no | the 150-track project is not in the fixture |
| S30 freeze | no | **yes** | passed, but the fixture is the small project, so treat as weak |

Done check passed: 5 of 28 (S04, S11, S12, S16, S30). S30 is weak.

The "stuck export" in S22 was the driver, not the app: it advanced the app
only on actions, so a finished export never reached the screen while the agent
watched. `look` and `tree` now step the app, with a test that fails without it.
