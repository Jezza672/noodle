# M7: Time warping

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** All four PRs and a round of follow-ups are done (the model and Repitch playback; the
engine with Grain and Beats; the clip editor; warp quantise and grooves, below). The
decisions are in CONTEXT.md (warp, clip tempo, beat and warp markers, groove,
section) and ADR 0002 (warping is a clip feature, not nodes).

Audio clips get a warp, applied live on playback. Four PRs:

1. **The model. Done.** A clip's warp (its clip tempo curve and warp markers,
   `ClipWarp`) is separate from the sections that show it on the timeline, and
   sections that show one clip share it by ID. An edit to a shared warp forks
   it for that section (`Project::warp_edit`), so copies stay as they were.
   Clip tempo is seeded from the tempo map for takes recorded in Noodle
   (conversion off, so they sound as recorded) and saved in the project;
   detection, tapping and typing arrive with the editor. Warp markers pin a clip
   moment to a timeline beat; a hidden tempo conversion maps clip tempo to the
   timeline's. Tempo conversion and warp markers are separate switches (warp
   quantise is the third, in PR 4). The only mode is Repitch, played by the
   track input with cubic interpolation, driven by a piecewise-linear map from
   output frames to source frames. Fades stretch with the audio. (Dragging trims
   and fades, and the loop, were disabled here and arrived in PR 3.)
2. **The engine. Done.** Grain and Beats are rendered by a worker thread
   (`noodle-warp`, one per track input) ahead of the playhead into a ring buffer
   per stretched section; the audio thread only copies out of it. Grain is
   waveform-similarity overlap-add (WSOLA); Beats cuts the clip at its
   transients (`noodle_io::detect_onsets`), plays each attack at natural speed
   and stretches only what follows. Seeks, jumps, stops and edits restart the
   stretch at the new place, and the loop's start is rendered ahead so a wrap is
   gapless. `ClipFeeds::set_preview` plays every warped section as cheap
   Repitch while a marker is dragged, so the editor (PR 3) can use it. Tests
   cover pitch, level, stereo balance, block-size independence, seeks, loops and
   the allocation guard (`stretched_clips.rs`, `stretched_playback.rs`,
   `track_input_realtime.rs`); `examples/warp-grain.ron` and `warp-beats.ron`
   have golden renders; `cargo bench -p noodle-nodes --bench warp` measures the
   stretcher (about 150 times real time per clip on one core). Choosing the
   mode in the app waits for the clip editor.
3. **The clip editor. Done.** In the piano roll's slot (double-click an audio
   clip): the clip's own timeline with its waveform, transient ticks, warp
   markers along the top (drag to move, double-click to add, Delete removes, snapped
   to beats; Alt drags free) and the loop brace. Marker drags play as cheap
   Repitch (`set_preview`), and the node now dips when that flips. The
   properties panel has the mode, the two switches, clip tempo (typed, tapped
   or detected from the file's transients by `noodle_io::estimate_tempo`, over
   its first 120 s), loop, gain and fades. Trims, fades, split and slip work on
   warped sections again, in warped beats. **Loop:** `ClipWarp::loop_region`
   (in clip-tempo beats, at least one beat and 0.25 s of playback, or the
   section plays once and the app says so) and `AudioClip::extent` (how far the
   section is dragged out, cutting it short or extending it). A looping section
   always plays from its start to the loop's end and then round the loop, up to
   its end, so dragging the edge never changes what plays before it. A fade-out
   is held to the last pass. The engine lays a looped section out as consecutive
   pieces, each a normal scheduled clip, so the audio thread has no loop logic
   and the repeats are gapless (stretched ones through the head lanes). Pressing
   Warp on an unwarped clip picks Beats for percussive audio (from the
   analysis) and Grain otherwise; the mode can be changed after.
4. **Warp quantise and grooves. Done.** `ClipWarp::quantise` (`WarpQuantise`:
   grid in beats, amount 0 to 1, an optional groove and how much of it) moves
   the markers that *play* (`ClipWarp::effective_markers`) and never the stored
   ones, so turning it off brings the original timing back; the curve, the
   engine and the render cache key all read the effective markers, and a marker
   that would land on or before its neighbour, or squeeze the clip past the
   speed limits, is left out of them. In the clip editor each flag sits where
   it plays, with a line and a ring back to where it was put; the properties
   panel has Quantise, Grid, Amount, Groove and Bake (makes the quantised
   markers the markers, `ClipWarp::baked`), and Pin transients (a marker on
   every detected transient where it plays now, so there is something to
   quantise and a feel to read). A `Groove` is the mean distance of a clip's
   markers from the sixteenth grid over a bar (`ClipWarp::groove`); a clip
   takes another's groove **copied** or **linked**. A link is clip data, not a
   wire (ADR 0002): `groove_from` names the other *clip* (a warp forks when one
   section is edited), and the project refreshes the follower's groove whenever
   a warp or clip changes (`Project::sync_grooves`),
   so undo and redo need not record it. `examples/warp-quantise.ron` has a golden
   render.

5. **Follow-ups. Done.** The session tempo is a field in the transport (with
   the time signature), and View > Tempo lane shows the tempo map as an
   optional stepped lane to add, move and remove tempo changes. In the clip
   editor: **grid offset** (drag the waveform or the ruler, or the panel's
   Offset) lines the beat lines up with the transients, and the clip slips with
   them so it plays from the shifted line; **Detect** is on
   whenever there are enough transients and also sets the offset (a weak guess
   says so); flags can be picked as a group (Shift/Ctrl click, a box in the
   flags' row, Ctrl+A) and moved or deleted together; each transient has a
   handle that a double-click turns into a warp marker. Warped sections in the
   timeline draw a waveform rendered from the warp (`timeline/warped_waves.rs`).

Out of scope for now: pitch shifting, warping MIDI clips, a spectral mode.

**Done when:** you can drop a loop recorded at another tempo, pin its
transients to the grid, hear it follow tempo changes without dropouts, quantise
it non-destructively, and lend its groove to a second clip.
