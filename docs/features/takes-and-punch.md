# Take folders, loop recording, punch, count-in and pre-roll

Planned in the roadmap (settled in #281), built in #289 (batches 1 and 2) and
#290 (comping in the arrangement, across linked tracks, and MIDI takes).

**What landed.**

- **Take folders.** A take folder is an ordinary audio clip with a
  `TakeFolder` (`noodle-core/src/clip.rs`, `AudioClip::takes`): a list of takes
  and a comp. All takes are windows into the clip's one file. The clip's
  `offset` and `length` are counted in the *first coordinates* (the file as the
  first take sees it), and take `k` plays `delta` frames later in the file for
  the same moment of the music, so trimming, splitting, copying, moving and
  linking the clip carry every take without any special case. The comp is a
  list of `CompSpan { at, take, crossfade }` in the same coordinates; a span
  plays until the next one. `TakeFolder::swipe` makes a take play over a range
  (cutting and merging spans); every comp edit is a `SetClip`, so one undo step.
- **Scheduling** (`noodle-nodes/src/clip/schedule.rs`, `lay_out_folder`). The
  folder is laid out as its plain window first, so a warp places it like any
  section, then cut into one piece per span, each reading its take's part of
  the file and butted to the next. Each join becomes a `Crossfade` on the later
  piece: the span's own (in ticks), or by default 10 ms (5 ms either side), laid
  out by the existing `apply_crossfades` with audio from both sides of the
  join, since the takes sit in one file. A span the file cannot supply is
  silent. Takes and comp are in the clip's cache key
  (`noodle-nodes/src/clip.rs`).
- **Flatten** (`timeline/comp.rs`, "Flatten takes" in a take folder's menu).
  The folder is cut at each join and each piece becomes a plain clip of its
  span's take, with the join's crossfade; unused takes are dropped (their
  audio stays in the file). Linked folders are flattened together and their
  pieces linked piece by piece. One undo step.
- **Loop recording** (`Session::start_take`, `stop_recording`). With the loop on
  and the take starting inside it (and no punch range), the take is one file
  of passes through the loop; `loop_takes` makes each pass a take, `delta` one
  cycle apart. The first pass keeps from where recording started to the loop's
  end; a final pass with less than a quarter of the window is dropped. A new
  comp plays the last complete take. Stopping in the first pass makes a plain
  clip. The armed tracks' folders are linked, as before.
- **Punch range** (`Project::punch`, `Command::SetPunch`, saved with the
  project): its own band along the bottom of the ruler with draggable edges,
  and Punch on/off, in and out in the ruler's right-click menu. With it on, a
  take keeps only the range: recording starts with the transport and the clip
  is cut to the punch-in and punch-out (the file also holds the pre-roll).
  The take ends by itself at the punch-out and the transport plays on. MIDI
  played before the punch-in is not kept.
- **Pre-roll** (an app preference, "Pre" in the transport bar, 0 to 4 bars):
  with the punch on, Record starts the transport that many bars before the
  punch-in.
- **Count-in** (an app preference, "In" in the transport bar, 0 to 2 bars):
  before the transport starts, the metronome clicks for that many bars. It
  uses the project's metronome node and clicks even if it is switched off
  (`Transport::count_in`, `TransportControl::set_count_in`). The transport is
  stopped, so the Metronome node counts beats from its own frame counter
  rather than from ticks, which also works at the start of the timeline. With no
  metronome in the project the take starts at once and says so.

**Tests.** `noodle-core` clip tests (swipe, joins, validation, round trip) and
project tests (punch saved and undone); `noodle-nodes/tests/take_folders.rs`
(each take where the comp says, the join at equal power, a set join crossfade,
a trimmed folder, a warp placing the spans, a take the file cannot supply);
the `comped-takes` example and its golden render; the metronome's count-in tests;
`freeze.rs` (takes and comp in the cache key); `timeline/comp.rs` (flatten
sounds the same, links, undo); `session.rs` (a take per cycle on four
passes with the right samples, a plain clip when stopped early, punch with
pre-roll keeping only the range, nothing recorded when stopped in the
pre-roll, punch refused past its range, the count-in waiting and cancelling).

**Comping (#290).**

- **Take lanes** (`timeline/takes.rs`). "Show takes" in a take folder's menu
  opens one lane per take under its track. Each lane shows that take's waveform
  (or notes) and tints where the comp plays it. Drag across a take to swipe that
  stretch into the comp (snapped to the grid, Alt for free): one undo step.
- **Across tracks.** A swipe applies to the folder and every folder linked to it,
  by time rather than by file frame, so a comp across drum mics stays aligned
  even if their windows differ (`comp::swipe`).
- **Audition.** Click a take's lane to play that take alone, in place of the
  comp, on the folder and its links (`Session::set_take_audition`); click again to
  go back. It is a copy of the project fed to the engine, so the project, its
  undo history and exports are untouched.
- **Joins.** While the lanes are open each audio join has a handle on the clip;
  dragging it widens or narrows the crossfade (symmetric), on every linked
  folder, as one undo step (`comp::set_join`).
- **Recording again over a folder (#375).** A recording that starts where an
  audio take folder on the same track starts and is as long as its window
  (record from the start of the loop again) adds its passes to that folder as
  new takes, as part of the same `Batch`, instead of making a clip of its own
  (`session/take_merge.rs`). Takes are windows into one file, so the folder's
  file and the new recording are written out together as a new `take-NNN.wav`,
  the old file first so every `offset` and the warp still point at the same
  audio, and the folder's clip names it; the old file stays for undo. The comp
  is left as it was. A recording over a different stretch, or in a different
  sample rate or channel count, is a clip of its own as before.
- **Lanes open themselves (#423).** A recording that makes a take folder, or adds
  takes to one, opens its take lanes (`Session::opened_takes`,
  `takes::State::open_recorded`), so the takes to swipe from are on screen as
  soon as recording stops; "Show takes" in the folder's menu still opens and
  closes them.
- **MIDI takes.** A loop recording of MIDI makes a take per pass when the
  Takes or Merge setting (in the Q dropdown, an app preference) says Takes,
  and one merged clip when it says Merge. A MIDI take folder is a `MidiClip`
  with `MidiTakes` (see data-model.md) and uses the same lanes, swipe and
  audition. Editing the notes of a MIDI take folder by hand turns it into a plain
  clip; "Flatten takes" does so on purpose.

**Tests.** `timeline/comp.rs` (a swipe comps linked folders the same way and
renders the swiped take, in one undo step; linked folders with different windows;
join crossfade on every link; MIDI swipe, hand edit drops takes, flatten),
`midi_take.rs` (passes become takes; a sliver is dropped; a silent pass is
empty), the take lanes' gestures in `timeline/tests.rs`, and `session.rs` (a
loop recording comped by swipes plays and exports the comp's samples; recording
again over a folder adds takes to it, and over another stretch does not).

**Not done.** Starting a loop recording from outside the loop; a loop recording
that starts partway through the loop does not keep the earlier part of the later
takes; take lanes draw a warped folder's waveform unwarped.
