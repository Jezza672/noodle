# Assessing how hard each user story is

A draft framework for scoring the 30 stories in [stories.md](../usability/stories.md). Nothing is scored yet. The aim is a scorecard per story that can be measured mostly by machine, rerun as Noodle changes, and compared over time and against other DAWs.

## 1. The unit of measurement: a route

A story is not measured directly. It is measured through one or more **routes**: a concrete, scripted sequence of user actions that reaches the story's goal from a stated starting state, plus a check that the goal was really reached.

- **Start state.** Usually an empty project, or a named fixture (for example "a project with a 4-bar drum loop and a vocal track"). Fixtures live next to the scripts so every route starts from the same place.
- **Done check.** An assertion on the end state: the graph, the timeline, or the rendered audio (pitch, level, timing, loudness). Without it a route can "finish" without working.
- **Several routes per story.** Real users have several ways to do a thing (mouse, shortcuts, a preset, a script node). Each route is tagged, for example `default` (what a newcomer would find), `expert` (fastest known), `keyboard`, `scripted`. The scorecard reports the `default` route and the best route, so a hidden fast path doesn't hide a painful obvious one.
- **Status.** Each story is one of: `full` (a route reaches every part of the goal), `partial` (a route reaches part; the rest is listed), `workaround` (needs an external tool or a hack), `blocked` (not possible yet, with the missing piece named). A blocked story is still useful: it is a roadmap item with a user-facing reason.

## 2. Metrics

Grouped by the question each answers. Items marked **(auto)** can come straight out of a recorded route; **(semi)** needs a script author's tag or a second run; **(human)** needs a person or a naive agent.

### A. Effort: how much do I have to do?

| Metric | Definition | How |
|---|---|---|
| Action count | Clicks, key presses, drags, scroll gestures, text entries, each counted separately and in total. A chord like Shift+A is one key action. | (auto) |
| Keystroke-Level Model time | Estimated expert time using the standard KLM operator costs: keystroke 0.2 s, point 1.1 s, button press 0.1 s, hand move between mouse and keyboard 0.4 s, mental preparation 1.35 s before each new sub-goal. Gives a seconds figure that weighs a drag across the screen more than a key press. | (auto, with M placed by rule) |
| Pointing difficulty | Sum of the Fitts index of difficulty, log2(distance / target width + 1), over every pointing action. Catches tiny sockets, thin clip edges and long travel. | (auto, from widget rects) |
| Precision actions | Actions that need sub-target precision: dragging to an exact value, hitting a 1-frame region, landing on a small socket. | (auto, threshold on target size) |
| Typed values | Numbers or names the user must type, and whether they could have been picked instead. | (auto) |
| Context switches | Moves between views or panels (node editor, timeline, piano roll, mixer, dialogs), and modal dialogs opened. | (auto) |
| Wait time | Wall-clock time the user waits on the app: analysis, stem separation, rendering, freezing. | (auto) |

### B. Repetition: do I keep doing the same thing?

| Metric | Definition | How |
|---|---|---|
| Scaling with size | Run the route at two sizes (3 samples vs 24; 2 tracks vs 16; 2 takes vs 8) and report the extra actions per extra item. Ideal is about 0 (one gesture covers any N); bad is a fixed cost per item. This is the single most telling number for stories like S12, S24 and S29. | (auto, needs a size parameter on the route) |
| Repetition ratio | How compressible the action log is: the share of actions that are a repeat of an earlier sequence of 3 or more actions. High means the user is doing the same chore by hand. | (auto) |
| Batch coverage | For each repeated chore, whether a multi-select, duplicate, "apply to all" or template path exists. | (semi) |

### C. Rework: do I ever have to throw work away?

| Metric | Definition | How |
|---|---|---|
| Forced rework | Actions spent recreating something that already existed, because a step destroyed it or the app required a different order (for example deleting and re-adding a node to change a setting that is not editable after creation). | (semi: the author marks recreate steps; auto-detect as add-after-delete of the same kind) |
| Undo needed | Number of undos a route needs to recover from the app's own behaviour, not from user mistakes. | (auto) |
| Undo fidelity | After the route, undo all the way to the start and redo all the way back; the state must match at both ends. Also counts how many undo steps one user action takes (one is ideal). | (auto) |

### D. Changeability: how hard is it to change an early decision later?

This is the "destructiveness" question. Each story lists a few **late changes** that revisit an early choice after the whole story is done, for example:

- S12: replace one sample; add three more notes to the folder; change the root-note naming scheme.
- S07: move the bridge before the second chorus after automation is drawn.
- S23: the label asks for −16 LUFS instead of −14 after mastering.
- S05: the project tempo changes from 124 to 128 BPM.

Each late change is its own short route from the finished state, with its own done check. Measured:

| Metric | Definition | How |
|---|---|---|
| Change cost | Actions for the change, and the ratio to the original route's actions. Under 0.1 is good; near 1 means "start again". | (auto) |
| Loss | Did the change throw away anything the user made after the decision (automation, edits, comp choices, plugin settings)? Checked by diffing the parts of the state the change should not touch. | (auto) |
| Premature commitment | Decisions the user must make before they have the information (choosing voice count before playing, sample rate before recording, stem names before export). Listed per story. | (semi) |
| Destructive steps | Steps that bake something in (render in place, bounce, apply, consolidate) without keeping the source, and whether the source can be recovered. | (semi) |

### E. Findability: can I work out how to do it?

A scripted route is written by someone who already knows the app, so it measures expert efficiency, not discoverability. These cover the gap.

| Metric | Definition | How |
|---|---|---|
| Surface of each action | Tag each action by how its entry point shows itself: visible button or label, menu item, context menu, search, keyboard shortcut only, modifier + drag only, documented nowhere. Score the share of hidden entry points. | (auto, if widgets carry the tag) |
| Vocabulary match | For each feature a route uses, whether searching for it under the names other DAWs use finds it ("sidechain", "duck", "bounce", "freeze", "comp", "multisample", "keyswitch"). A list of synonyms per feature is checked against the add-node and command search. | (auto) |
| Naive success | Give a fresh agent (or a person) with no Noodle knowledge only the story text and the running app, and record success, time, actions, wrong turns and dead ends. | (human, or agent; periodic, not per PR) |
| Error proneness | Plausible slips that are accepted silently and lead to a wrong result (wiring the wrong port, dropping on the wrong track). Found by naive runs and listed. | (human) |

### F. Feedback: do I know it worked?

| Metric | Definition | How |
|---|---|---|
| Time to hear | Actions and seconds from the first action to the first moment the user can hear a meaningful result. | (auto) |
| Live editing | Whether each step works while the transport runs, or forces a stop. Matters most for S20, S27 and S28. | (auto: run the route with playback on) |
| State visibility | Whether the result is visible without opening another view (the warp markers, the sidechain amount, which take is active). | (semi) |

### G. Robustness

| Metric | Definition | How |
|---|---|---|
| Glitch-free | Audio dropouts or xruns while the route and its late changes run, at the story's stated size. | (auto, with the virtual output) |
| Save and reopen | Save, reopen and check that the done check still passes and the state is identical. | (auto) |

### Other common usability measures worth knowing

- **Cognitive Dimensions of Notations** (Green and Petre). Designed for visual programming tools, so a close fit for a node editor. Several of the metrics above come from it: viscosity (resistance to change), premature commitment, hidden dependencies, error proneness, hard mental operations, progressive evaluation (can you try it half-built), visibility and juxtaposition, abstraction gradient, consistency. A one-page rubric per story could score the rest by hand.
- **GOMS and KLM.** The model behind the timing estimate above.
- **Nielsen's ten heuristics.** A quick expert pass per story: visibility of status, match with the real world, user control and freedom, consistency, error prevention, recognition over recall, flexibility, minimalism, error recovery, help.
- **Standard usability-test measures:** task success rate, time on task, error rate, and learnability (the drop in time between a user's first and third attempt).
- **Questionnaires for people:** System Usability Scale (10 questions, 0 to 100) for the app as a whole; NASA-TLX for how demanding one task felt; Single Ease Question (1 to 7) after each task. Only useful once real users try the stories.

## 3. Making it automatable

The app already has what this needs. `crates/noodle-app/src/testkit.rs` drives the UI with real pointer and key events through `egui_kittest`, and `crates/noodle-app/src/acceptance.rs` already builds a patch "as a user would". A story route is the same kind of test.

1. **Instrument the testkit.** Every helper (`click`, `drag`, `press`, `drop_files`, text entry) appends an entry to an action log: kind, modifiers, start and end point, the widget hit and its rect, the view it is in, and the time. Nothing else in the app changes.
2. **Write each route as a test** under something like `crates/noodle-app/src/stories/s12_multisample.rs`, with its fixture, its done check, its size parameter, and its late changes as further tests that start from its end state. The route is written as a user would: it may only use the testkit helpers, never call into `Session` or issue `Command`s directly, so it cannot cheat past the UI. Reading state for the done check is allowed.
3. **Compute metrics from the log.** A small crate or bin turns logs into a JSON scorecard per route, and a Markdown summary table across all stories.
4. **Run in CI.** The routes are tests, so a UI change that breaks a route fails CI and must update it in the same PR. A separate step posts the scorecard diff against `main` on the PR ("S12 default route: 41 → 23 actions; S07 change cost 0.6 → 0.1"), so the effect of each change on users is visible in review.
5. **Track blocked stories in the same place.** A blocked or partial story is a route file with its status and the missing feature named, so the table always covers all 30. When a feature lands, the PR that adds it updates or adds the route. That makes it a "docs move with code" rule, which can go into CLAUDE.md.
6. **Naive runs, periodically.** Once a month or per milestone, a fresh agent with no Noodle context gets the story text and drives the app through the same harness (screenshots plus the accessibility tree egui exposes), with an action budget. It records success, actions and the places it got lost. This is the only automated measure of findability, and its dead ends are a good source of issues.

### What is built

- **Action log** (`testkit.rs`). `testkit::record()` turns recording on for the thread; every helper then logs an `Action` (kind, modifiers, start and end point, the widget under each end from egui's accessibility tree, the view each end is in, frame time). The app marks its panels with `testkit::mark_view`, so views are `node editor`, `timeline`, `piano roll`, `clip editor`, `mixer`, `outputs`, `scope`, `properties`, `menu`, `status`, or `overlay` for a dialog, menu or popup. Off by default, so other tests are unchanged.
- **Routes** (`noodle-app/src/stories/`, test-only). A `Route` has a story, a tag, a fixture, an optional size, steps and a done check; a `LateChange` starts from a route's end state. Steps get a `Handle` (input events only), the done check a `Probe` (read only). The registry (`registry.rs`) lists all 30 stories with a status and the missing piece; a test keeps it in step with `stories.md`. Routes live in `stories/routes/`, one file per story.
- **Effort scorer** (`effort.rs`, group A): action counts, view and dialog switches, wait time, and KLM time. The rules that place the operators are in the file's header.
- **Report** (`report.rs`): a JSON file per route and a Markdown table across the stories. With `NOODLE_SCORECARD_DIR` set the route test writes them; CI (Linux) does so and uploads the `usability-scorecard` artifact.

- **MVP routes** (`stories/routes/s01_comp.rs`, `s09_drum_pattern.rs`, `s12_multisampler.rs`, `s19_drum_bus.rs`, `s20_automation.rs`). S01 swipes two phrases out of an eight-take folder and re-picks one; S09 chokes the open hat with a Script node between the track and a Drum Rack, programs a two-bar pattern in the piano roll and extends it to four bars; S12 (sized by the number of recordings) drops the recordings' folder on the node editor, which makes a Sample Set, and plays it with one Sampler, then takes the replace-a-recording, add-three-notes, rename and bend-range late changes through Rescan and one parameter; S19 builds a drum bus from a Mix node, a pre-fader parallel bus from a second Mix and a hard Dynamics (switched to post-fader as a late change), and ducks a bass from the kick through Dynamics' sidechain; S20 automates a Gain, learns a knob on a control surface onto it, rides it in Touch mode while the transport runs, and trims the chorus by -2 dB with a second gain stage. S12 is `full`, the others `partial`; the registry names each gap and its issue. **When a story is possible but awkward, the route takes the workaround** rather than calling the story blocked, so the scorecard shows what the workaround costs; the issue stays open for the real fix. Fixtures are built by hand, then saved and reopened (`routes::finished`) so the undo stack starts empty.
- **The other routes** (`stories/routes/`, one file per story, S02 to S30). Every story now has a route, and the registry says how far it gets. Stories made of nodes (S13 to S17, S22, S23) use a small script language in `routes/patch.rs`: a list of `Op`s (add a node by searching for its type, wire two sockets, type a parameter, play) that both drives the steps and is checked by the done check, so a route says what the patch is once. Piano roll stories (S08, S10) share `routes/roll.rs`; stories that start from recorded audio (S05, S06, S23) share `routes/audio_clip.rs`. S02 sets the punch range from the ruler's menu (`Probe::ruler_at` finds the ruler). S06, S18, S23 and S24 stop where a native file dialog, a model download or a missing plugin would take over, since a route cannot drive those; their done checks read the project, because the save-and-reopen check runs the done check on a reopened project that has no dialogs. **Blocked stories have a tripwire** (`routes/tripwire.rs`): the route opens the File, Edit and View menus and looks for the feature's name, and passes while it is absent. The day a menu offers it the route fails and says so, and the PR that adds the feature writes the real route and moves the registry status. S10 is `full` (articulations from the piano roll's menu through an Articulation Map) and S21 `partial` (it stops before the batch export's folder dialog); S30 is `partial` for the CPU view and the 60-track size (#369).
- **Control surface and code.** `Handle::control` moves a knob on a MIDI control surface by sending CC on the session's MIDI bus, which the app hears as it would a device; it is logged as a `Control` action (a P, on its own hand). `Handle::type_code` types a program into a Script node; it is logged as `Code` and costs a K per character plus an M per statement, and the statements are counted in the scorecard's Code column, because writing code needs knowledge of the script language that no time figure captures.
- **Running one story.** `NOODLE_ROUTE=S12 cargo test -p noodle-app every_route` runs only that story's routes.
- **Repetition scorer** (`repetition.rs`, group B). A route with a `size` runs again at twice that size; the extra actions and KLM seconds per extra item go in the scorecard. The repetition ratio and batch-path notes are not built yet.
- **Rework scorer** (`rework.rs`, group C). Counts the actions in steps the author marks with `Handle::recreating`, flags a node kind added after one of that kind was removed (the `Handle` watches the graph after each action; undo and redo are not counted), and counts undo chords. After each route it undoes everything and redoes it, and checks both ends against the project as it was; a mismatch shows as "UNDO ROUND TRIP FAILS".
- **Late-change runner** (`mod.rs`, group D). Each `LateChange` runs from the route's end state and reports its actions and KLM time as a ratio of the route's. It declares what it must leave alone as `Keep` entries (a named projection of the project as text); one that differs after the change is reported as lost. Premature-commitment and destructive-step tags are not built yet.

- **Entry-point surfaces** (`surface.rs`, group E). Each action is put in a surface: visible, menu, context menu, search, shortcut only, modifier only, or untagged. The prototype found that widgets cannot carry a tag without touching every call site (menu bodies exist only while open, and shortcuts are consumed in about thirty places), so pointer actions are classified from the widget the accessibility tree shows under them, and key chords from a table (`CHORDS`) that says how else each can be reached. A chord not in the table is `untagged`, and a test keeps every global shortcut in it. Hidden entry points are shortcut-only plus modifier-only actions over all actions; untagged ones are listed per route in the JSON.
- **Vocabulary match** (`vocabulary.rs`, group E). Each name other DAWs use for a feature is typed into the add-node search (the same function the search box calls) and the top three are compared with the right node. Features that are not nodes are reported as not searchable, since there is no command search. The report is part of `scorecard.md` and `vocabulary.json`.
- **Feedback** (`feedback.rs`, group F). *Time to hear:* `Probe::hear` logs what it caught; the first listen with sound sets the actions and KLM seconds before it (a route that never listens is reported so). *Live editing:* the route is run again with the transport already playing; its own Space presses are skipped, the transport is stopped before the done check, each step that stops the transport is named, and a failing step or done check is reported (not asserted). A route whose playhead has run past its material can read as silent live. *State visibility:* `Handle::unseen` tags a step whose result needs another view.
- **Robustness** (`robustness.rs`, group G). Output underruns counted by the engine across the route and its late changes (reported, not asserted, since a loaded CI machine can drop a block). After each route the project is saved and opened again; it must read back identically and the done check must pass on it, which fails the route.
- **Diff on PRs** (`scripts/scorecard_diff.py`, the "Scorecard diff" step in `ci.yml`). The baseline is the `usability-scorecard` artifact of the latest green `main` run, not a committed file, so PRs never conflict on it. On each same-repo PR the Linux job diffs `scorecard.json` against it and posts one comment, updated in place (found by a hidden marker), listing only the route rows that moved beyond tolerance: whole-number metrics report any change, KLM and wait seconds 0.5 s, ratios 0.02. With no baseline yet, or no change, the comment says so. Fork PRs have a read-only token and skip the comment (the diff still goes to the job summary). The script has its own tests, run in the same step.

- **Human rubric** ([usability/rubric.md](../usability/rubric.md)). Six Cognitive Dimensions rated by hand per story (filled in for the MVP stories), and the protocol for human sessions: Single Ease Question after each story, SUS at the end, NASA-TLX optionally, results in `docs/usability/sessions/`.
- **Registry statuses.** Every story has a status. The 24 not yet routed were assessed from the docs and the code, so their statuses are a first reading, and a route may move them either way.

### Keeping it current

- **New features:** the route file is the record. A feature that makes a story easier should come with a new or shorter route, and the scorecard diff shows the gain.
- **Several ways of doing something:** handled by tagged routes. The scorecard always shows `default` and `best`; adding a faster expert route never hides a slow default.
- **Drift in the stories themselves:** stories have stable IDs and are never renumbered, so history stays comparable.
- **Comparisons with other DAWs (optional):** for a handful of stories, count the same route by hand in one or two other DAWs from their manuals, as a reference line. Cheap to do once and useful for deciding what "good" looks like.

### What can't be automated well

Findability for real people, how frustrating something feels, and sound-quality judgments. Those need the naive-agent runs, the Cognitive Dimensions rubric, and eventually a few human sessions with SUS and Single Ease scores.

## 4. The scorecard

One row per story, one column per metric group, no single combined score. A combined number hides the reason a story is hard, and the reasons are what turn into issues. If a summary is wanted, give each group a grade (A to E) against thresholds set after the first full pass, once there is real data to set them from.

| Story | Status | Route | Actions | KLM s | Per extra item | Repetition | Rework | Worst change cost | Loss | Hidden entry points | Naive success |
|---|---|---|---|---|---|---|---|---|---|---|---|
| S12 | ? | default / best | | | | | | | | | |

## 5. Decisions and plan

The decisions and the plan are in issue #306 (the map).
