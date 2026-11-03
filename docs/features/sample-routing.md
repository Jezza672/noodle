# Sample routing

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** done. Closes #212, #213, #214 and #215.

1. **Sample connection type (#212).** A fourth port kind, the sample reference
   (`hash` or `hash:start:end`). Resolved at compile time into the receiving
   node's config, so nothing about it runs on the audio thread. Crosses group
   boundaries like any wire. New `Sample` node (`noodle.instrument.sample`);
   the Sampler's `sample` option is also an optional input that overrides it.
2. **Drag a clip or track into the graph (#213).** A clip dragged out of the
   timeline onto the canvas becomes a `Sample` node holding the part of the
   file the clip plays (a snapshot, not a link). A track dragged out is placed
   where it is dropped, at the level being edited.
3. **Drag a clip onto a Sampler (#214).** On the node or the properties panel:
   sets the sample, `start` and `end` from the clip, and the root.
4. **Fundamental from the first 0.3 s (#215).** `noodle_nodes::fundamental`
   (YIN, median of the frames that agree, "no pitch" for noise and drums),
   cached by hash in the `SampleStore`. Used whenever a sample is chosen for
   a Sampler, including Load sample.
5. **Not in scope.** The clip's warp is not carried (the window is the part of
   the file, played unwarped); the Sampler's waveform view shows its own option,
   not a wired sample.
6. **One audio pane for clips and samples (#231).** The sample view and the
   unwarped clip editor share `clip_editor/pane.rs`: seconds ruler, zoom and pan
   (also middle-drag), the sample curve when zoomed in, the lit part with draggable
   start and end, loop ends, and slip. A clip's edges trim it; a Sampler's set
   `start`/`end`; dragging the audio slips the clip's offset or moves the
   Sampler's region.
7. **Clip and sample parity.** "Anything you can do to a clip you can do to a
   sample", and the other way round. A Sampler's sample now opens in the clip
   editor itself (`clip_editor/subject.rs`: one `Subject`, a clip or a Sampler,
   with the same commands either way), and gained what a clip had:
   - a **gain envelope** (`gain_env`, drawn and edited on the waveform);
   - a **playhead** for each sounding voice (Sampler voice positions reported
     through `Telemetry::open_positions`);
   - a **warp** (`warp`: tempo, follow tempo, markers, grid offset, quantise,
     Detect and Tap, edited on beats in the same view, with a Warp section in
     the Sampler's properties). The Sampler plays it as Repitch;
   - and the other direction, a **loop on an unwarped clip** (a natural-speed
     warp that holds the loop; the clip stays in the seconds view, with the
     toolbar's Loop checkbox and pullable loop ends).
   Not carried over then: Grain and Beats warp modes for samples, the gain
   envelope in the beat view, and a sample loop shorter than a beat in the
   beat view. Grain and Beats followed (see "Shared warp engine" below).

## Shared warp engine for samples

A Sampler now has the clips' warp modes. Grain and Beats run the clip engine's
`Stretcher` over the whole sample on a worker (`sampler_stretch.rs`), and the
voices play the stretched copy with the existing pitch, loop and envelope logic,
mapped back to the sample through the clip's `WarpMap`. The mode picker is shown
for samples. While the copy is made, and while a marker is dragged, the warp
plays as Repitch; an offline render waits for the copy. Also in this change:

- `gain_env` and `warp` are *live config* (`NodeType::live_config`/`retune`), so
  dragging an envelope point or a marker no longer rebuilds the Sampler and cuts
  its notes.
- `Telemetry::read_positions` lists the voices of every instance still
  reporting (a Sampler inside a voice-instanced group, or two instances across a
  plan swap) instead of the newest instance's only.
- `DSP_GENERATION` is 6, since a Grain or Beats warp on a Sampler used to render
  as Repitch.

## Random Select (#211)

A primitive node, `noodle.util.randselect`: one output routed from one of N inputs,
chosen at random on each trigger. Signals or notes (config `kind`), uniform pick
with an optional "don't repeat" and a seed, instant or zero-crossing switching
for signals, and finish-or-cut for held notes. Design in
[synths-and-library.md](../architecture/synths-and-library.md). Tests:
the node's unit tests (picks, seed determinism, avoid repeat, zero crossing,
held notes) and `realtime.rs`.

Random Select's editor behaviour is done, in "Multisampled instruments" below.

## Multisampled instruments (#331)

**Sample Set** (`noodle.instrument.sample_set`) holds a list of recordings and
outputs the whole list on the sample wire. Each entry is a sample, the key it was
played at, a velocity layer and a round-robin slot, as text
`hash[:start:end]@root[/layer][#rr]` joined by `;`, so it keys the cache and
needs no new wire kind (`resolve_samples` writes it into the Sampler's `sample`
like any other reference). Its other config: `names` (the file names, so a
rescan can tell what changed), `folder` (where they came from) and `middle_c`
(C3 or C4 is MIDI 60, used when reading names).

- **File names.** `parse_name` takes three things from a name, in any order and
  with any separators: the note (`C#4`, `Db4`, `61`), the layer (`ppp` to `fff`,
  `v1` to `v16`, `vel100`) and the take (`rr2`, `t3`). A name it can't read a
  note from leaves the root empty, shown as `?` with a warning, never guessed.
  Layers are ranked per note, softest first, so they stay ordinal and the ranges
  stay derived. The table also warns where the sound's detected pitch is more
  than a semitone from the root (the C3/C4 mistake).
- **Sampler set mode.** Given a set, a note picks the nearest root that has any
  recording (ties to the lower), then the velocity layer (a key's layers split
  1 to 127 evenly; `vel_curve` bends the velocity first), then the take
  (`rr_mode`: Cycle, Random or No repeat; random uses the node's seed, so renders
  repeat). All of it is lookups in tables built with the node
  (`SetTables`), so a note-on allocates nothing (`realtime.rs`). A set plays as
  Repitch, from each recording's own root, without the gain envelope or a warp.
- **`bend_range`** (semitones, default 2, 0 to 48) is new on the Sampler: it
  scales channel pitch bend into the per-voice bend. A per-note pitch
  expression stays in semitones.
- **Filling a set.** Drop a folder or files from the desktop on the node editor
  (on a Sample Set they are added; on the empty canvas they make one), or use
  Load folder and Add files in its properties. A folder is remembered.
- **Rescan folder** reads the folder and opens a dialog listing what is Added,
  Replaced (same name, new audio), Removed and Renamed (same audio, new name:
  nothing to do), plus names with no readable note. Apply (or Enter) is one undo
  step; Cancel changes nothing. Rows you edited keep their edits unless their
  file was replaced.
- **Multisample Instrument** is a system template: Sample Set into a Sampler
  (gate mode) into a low-pass, with `notes`, `tune`, `bend range`, `release`,
  `gain` and `cutoff` as group inputs.
- **Random Select takes any connector type.** Until something is wired to its
  data ports it has no type: they take any wire, the type-specific parameters
  are hidden and the spare input is greyed. The first wire sets `kind` in the
  same undo step; once typed, only that type can be wired (the ports are
  typed, so a drag is refused while it is made). Taking the last wire off
  returns it to untyped. Inputs grow with the wires, as a mixer's do. Spectral
  and sample wires are refused with the reason: a sample wire is resolved when
  the project is compiled, so nothing can switch it while audio runs.
- **S12** goes from `partial` to `full`: the route drops the folder on the
  canvas, plays the set with one Sampler, and its late changes (replace a
  recording, add three notes, change the naming scheme, change the bend range)
  go through Rescan or one parameter. Tests: the name parser, tables and rescan
  (`sample_set.rs`), the Sampler's set and bend tests, the session's folder and
  rescan tests, the editor's Random Select tests, `realtime.rs`.
