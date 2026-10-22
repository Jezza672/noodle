# Time warping

An audio clip can be **warped** (CONTEXT.md; ADR 0002 says why it is not nodes).
The document side is in `noodle-core/src/warp.rs`: a `ClipWarp` holds a
`TempoCurve` (the clip's tempo along its own seconds, a step function), the
`WarpMarker`s (each pins a moment of the clip, in source seconds, to a beat),
the `WarpMode` (Repitch, Grain or Beats) and the switches `tempo_conversion` and
`use_markers`. It lives in the project's `warps` map and an audio section
(the code's `Clip`) names it by `WarpId` in `AudioClip::warp`; sections that
show one clip share an ID, `Project::warp_edit` forks it when a section is
edited on its own, `AddWarp`/`SetWarp`/`RemoveWarp` are commands, and a warp
nothing uses is left out of the saved file. The format is 3; older files load
(`warp` and `warps` default to none). A file that uses Grain or Beats can't be
read by a build from before those modes existed (it doesn't know the name).

`ClipWarp::curve` is the one definition of where a moment of the clip plays: the
clip's beat at a moment is the integral of its tempo, the markers bend clip
beats into warped beats (straight lines between markers, the clip's own tempo
before the first and after the last), and with the conversion on the result is
in timeline beats, so the clip follows the tempo map. With it off the beats are
turned back into seconds at the clip's own tempo, so the clip keeps its natural
speed and the markers still re-time it. Markers must rise in time and beat and
may not squeeze or stretch the clip beyond 16 times (`MIN_WARP_SPEED`,
`MAX_WARP_SPEED`). Tempo conversion multiplies that by timeline bpm over clip
bpm, so the engine also caps the speed it reads the file at (`ENGINE_MAX_SPEED`,
32 times) when it builds the map: the section's fast parts play at the cap and
fall behind, because the disk stream can't supply more. A
section starts playing at its `offset` into the file, wherever that falls in the
warped clip.

The engine turns the curve into a `WarpMap` per section
(`noodle-nodes/src/clip/warp.rs`, built by `ClipFeeds::update` from the
`TempoTable`): knots of (output frame, source frame), straight between, with a
knot at each marker, clip tempo change and timeline tempo change in range, so it
is exact. A warp that leaves the clip at natural speed (`is_identity`) builds
no map and plays as an unwarped clip, bit for bit. The section's length is where
the map ends; fades are stretched through it.

The track input plays a warped `ScheduledClip` from the same disk streams as
any other, which are always at the engine's rate and in source frames.
`play_warped` renders up to 256 output frames at a time: it works out the source
frames they reach (plus one behind and two ahead), keeps them in a window it
reads from the stream (carrying it over from block to block, so the stream is
read once, in order), and interpolates each frame cubically (Repitch: pitch
follows speed). Seeking, the loop's head streams and parking use
`ScheduledClip::stream_start`, which gives the source frame for a clip frame.
Ends hold their edge frame for the interpolation, so cutting a clip doesn't
ring. Everything is preallocated at instantiation (`realtime.rs` plays warped
clips, edits them while they play and loops over them under the counting
allocator). The warp is in the track's cache key when it changes the sound.

**Grain and Beats** keep the pitch, which needs more than the audio thread can
do, so they are rendered ahead (`noodle-nodes/src/clip/ahead.rs` and
`stretch.rs`). A section whose warp has one of those modes is *stretched*
(`ScheduledClip::stretched`): the hub opens no stream for it, and a worker
thread (`noodle-warp`, one per track input node, like the hub) opens a **lane**
instead. A lane is a ring of stereo frames (about 340 ms) the worker fills with
the stretched section, and a small control block; the node holds up to
`MAX_LANES` of them, handed over and taken back through queues the way streams
are, so nothing is allocated or freed on the audio thread. The lane is keyed by
the clip, its source, the mode and the map (`lane_key`), so a new warp is a new
lane.

`play_stretched` asks for the frames it needs and copies them out. The node
tracks where the lane's next frame falls in the section; when the playhead is
somewhere else (a seek, a jump, a loop wrap, an edit) it asks the worker, through
the control block, to restart there. The worker says how many frames it had
pushed before restarting, and the node drops that many as stale. While the worker
catches up the playhead plays silence and counts an underrun, and the lane drops
the frames of the ring the playhead has passed, so it comes back on time. A
lane opened ahead for the loop's start (a *head*, as for streams) makes the wrap
gapless; offline (`ClipFeeds::blocking`) the node waits for the worker, so a
render comes out the same every time and whatever the block size.
`ClipFeeds::set_preview(true)` makes every warped section play as Repitch from
the hub's streams, which is cheap enough to follow a marker being dragged;
turning it off brings the lanes back.

The **stretcher** (`stretch.rs`) is WSOLA over the section's `WarpMap`, run on
two bands that are added at the end. The source is split by a linear-phase
low-pass at 150 Hz (`Split`: a triangular anti-alias filter, every eighth
sample, a 511-tap FIR at that rate and cubic interpolation back up, a block at a
time and cached); the bass goes to one band and the rest (source minus bass) to
the other. One set of grains can't serve both: the shift that lines up a
vocal's period is a random phase jump for the bass under it, which detunes and
pumps a bass line. Each band has Hann grains read from the source at natural
speed where the map puts it and moved to the offset that best continues the
grain before (normalised correlation of the mixed-down audio, coarse then
fine). The upper band's grains are 40 ms (hopped by 20 ms in the output) and
move up to 10 ms; the bass band's are 100 ms and move up to 12.5 ms, half a
period of a 40 Hz note. All channels use the same offsets; the output is
stereo. A grain that would run past its part of the source is moved back to
repeat the last stretch of it, so the end of the section doesn't go silent.
After a seek it renders three hops it throws away.

Each band is **cut at transients**, and what is cut at differs. The worker finds
two lists once per source and keeps them: the transients of the whole mix
(`detect_onsets`: a block's level against the average power of the 30 ms
before it, not the quietest of two short blocks, which a low held chord's
swinging block power fooled into an attack every few blocks) and those of its bass (`detect_bass_onsets`: the level of the
low-passed audio, rising at once and falling over 40 ms, so that a low tone's
swing isn't an attack). **Beats** cuts the mids and highs at the first list and
the bass at the second; **Grain** cuts both at the bass's, so that a kick's body
and the click on top of it stay together and land where the map says, and a
sustained note isn't cut. The map says where each transient lands in the output
and each segment is rendered on its own: its first 25 ms are copied from the
source at natural speed with the search switched off, and a 1.5 ms crossfade
ends on the transient. A segment's source is cut to the segment before the bass
is filtered, so the next hit's bass can't leak in ahead of it. The stretcher has
no I/O; the worker feeds it from its own stream through a window of decoded
frames (`StreamSource`).

**Loops.** `ClipWarp::loop_region` is a `LoopRegion` in clip-tempo beats (at
least `MIN_LOOP_BEATS`, 1, and a round of it at least `MIN_LOOP_SECONDS`, 0.25 s
of playback, else the section plays once and `ClipFeeds::update` reports a
problem) and `AudioClip::extent` is how far the section is dragged out; without
one the window is the length. A section with a loop always plays origin to the
loop's end and then the loop, up to its length. `ClipFeeds::update` (`lay_out`
in `schedule.rs`) expands it into **pieces**: the first runs from the section's
origin to the loop's end, then the loop repeats and the last is truncated at the
section's end (`WarpMap::truncated`); a section that ends inside the first pass
is the window cut off. Each piece is an ordinary `ScheduledClip` with a
synthetic ID (`piece_id`; at most `MAX_PIECES`), so the hub opens streams for it
a second ahead and the node needs no loop logic. The fade-in is on the first
piece and the fade-out on the last, held to the last pass.

**The clip editor** (`noodle-app/src/clip_editor/`, `clip_panel.rs`) edits a
`ClipWarp` through `Project::warp_edit` commands, one undo step per drag. The
timeline's trims, fades, split and slip of a warped section work in warped beats
(`timeline/warped.rs`). While a marker is dragged the session turns the preview
on (and off at the end of the drag); the node dips (declick) when it flips.
A clip that isn't warped opens too (`clip_editor/plain.rs`): its file on a
seconds ruler with the part the clip plays lit, where dragging the waveform
slips the audio under the clip (a `SetClip` of the offset, one undo step),
pulling either edge of the lit part trims the clip (the timeline's
`trim_start`/`trim_end`), and a Warp button turns the beat editing on.
That seconds-axis view is `clip_editor/pane.rs`, shared with the Sampler's
sample: a clip and a sample are the same thing to it (a lit stretch of a
file, optionally with a loop inside), so zoom, pan, the sample curve when
zoomed in, slip and edge pulls behave the same on both. The caller turns the
pane's `Gesture`s into edits: `plain.rs` into `SetClip`, `sample_view.rs`
into the node's `start`, `end` and loop parameters. Beat warping, the gain
envelope and the playhead belong to a clip's place on the timeline, so only
clips have them. The toolbar over the piano roll, the clip editor and the
sample view is `toolbar.rs`: a title and a Close button that stay in view
however long the hint is. The Sampler's sample (`sample_view.rs`) is drawn as
a preview in its properties, and a double-click opens it in the same slot.

**Grid offset and the clip editor's gestures.** `ClipWarp::grid_offset` slides a
clip's beat lines (not its markers or tempo) by a number of beats, so the lines
can be lined up with the transients of a loop whose first beat is not at the
start of its file. Things that read the grid care: the editor's snapping and
ruler, warp quantise (`WarpQuantise::apply_from`) and the groove
(`ClipWarp::groove`). A grid nobody hears would line nothing up, so every
change of it also slips the section along the file by the same number of beats
(`clip_panel::slide_grid`, through `Project::section_edit`): the section starts
at the same place against the lines, so the audio that plays is the audio
under the new lines, as when a waveform is dragged against the grid in
Ableton. The warp and the section's `offset` and `length` change in one
command (one undo step, and the warp forks first when it is shared); the
slip stops at the ends of the file. The offset reaches the sound only through
that slip and through markers moved by warp quantise, which the cache key
already hashes. Dragging the waveform or the ruler in the clip
editor sets it, as does the panel's Offset field and Detect
(`noodle_io::estimate_grid` gives the tempo and the phase of the beat, even as
a weak guess unless `TempoEstimate::is_clear`: onsets mostly on the beat, or a
beat standing well above every other tempo's score; `Analysis::of` scores the
bass onsets as well as all of them, since hats and arpeggios bury the beat in a
full mix, and the treble's (`detect_treble_onsets`) when neither is clear, for
bells over a dark pad; Detect is enabled whenever there is any). Flags are picked as a set (`ClipEditor::selected`: click,
Shift/Ctrl click, a box in the flags' row, Ctrl+A) and a picked group moves and
is deleted as one edit. Each detected transient has a handle on the row under
the brace; double-clicking it pins a marker where the transient plays now
(`pinned_transient`), so nothing changes until it is moved or quantised.

**Warp quantise and grooves.** `ClipWarp::quantise` is a `WarpQuantise` (grid
in clip beats, amount, optional `Groove` and groove amount). The stored
`markers` are never changed by it: `ClipWarp::effective_markers` gives the ones
that play (each beat moved toward its grid line by the amount, plus the
groove's offset for that line), dropping any that would land on or before the
marker before or break the speed limits, so they always make a playable warp.
`marker_map`, `curve`, and so the engine and the app's layout, read them, and
the track input's cache key hashes them. `ClipWarp::baked` makes them the stored
markers. A `Groove` is per-slot offsets (in beats) over a repeating pattern;
`ClipWarp::groove` reads one from the stored markers (sixteenth slots, one bar).
`WarpQuantise::groove_from` links to another clip (not its warp, which forks when one section
is edited): `Project::sync_grooves` (run when a warp or clip is added, replaced
or removed, and on load) copies the groove of the warp that clip uses now into
the follower, reading only the source's stored markers, so links
never chain or loop. The groove is derived data and is saved with the follower,
so a link to a clip that is not saved keeps the last groove and is dropped on
load. Warp data is not a wire (ADR 0002); a groove is lent as clip data.
