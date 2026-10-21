# Synths, the library and effects

There are no synth nodes. A synth is a group (`noodle.group`) of primitive
nodes (oscillators, filters, envelopes, VCAs, `Math`), with its controls as the
group's inputs, so it stays editable once created: open it and change it.
An input left unwired leaves the node behind it at the value set inside.
`examples/subtractive-synth.ron` is one, and its unison is a nested group
(five `Saw`s, each detuned by a `Math` node and placed by a `Pan`, into a
`Mix`), so a unison of squares or of anything else is the same group with
another oscillator. `Math` (`noodle.util.math`) takes an
`expr` config such as `a * b + c` over inputs `a` to `d`; it is compiled to a
postfix program when the node is built and run on a fixed stack per sample.
`Pan` (`noodle.util.pan`) is the equal-power stereo placement it uses.
`Script` (`noodle.util.script`) is Math grown into a program: its `script`
config declares the node's ports (`input x`, `param cutoff = 1000 [20, 20000]
log hz`, `output y`), `state` variables that last between samples, `let`
locals, assignments, `if`/`else` and the Math functions plus `rand()`, `db()`,
`mtof()` and `select()`, with `sr`, `bpm`, `beat` and `time` readable.
`array buf[480]` is memory per lane (8192 values in all, indices wrap, so a
write head kept with `% len(buf)` makes a delay line; an `f32` head that
counts up for ever stops at 2^24 samples) and `for i in 0 to 8 { }` loops with
ends fixed in the text (whole numbers or `len(buf)`; 256 trips, 1024 steps for
nested loops, and 32768 ops a sample counting every pass, checked when the
script is read), so a script is still bounded and safe on the audio thread.
`events in notes` gives `notes_on`, `notes_off`, `notes_key` and
`notes_velocity` (one event per sample; a chord's notes arrive on consecutive
samples, a queue of 64 carries the overflow into the next block, via
`LaneKernel::begin_block`) and `events out played` takes `note_on(played, key,
velocity)` and `note_off(played, key)`; events have no voices, so the first
voice and channel's lane sends. The text is
parsed to a postfix program (jumps forward for `if`, back for `for`) when the
node is built (a mistake fails the node with its line number), and each lane
keeps its variables and arrays in fixed arrays, so nothing allocates. The script shapes ports, so
it is config, and it has no node presets (a preset keeps its node's ports). The
properties panel edits it as a code box that commits on losing focus, showing
the ports it makes or the error as you type. `examples/script-fold.ron` and
`script-comb.ron` (an array delay line and a loop) are two.
`Delay` (`noodle.util.delay`) delays its input by `time` (seconds, up to 2,
interpolated linearly, offset-style modulation so an LFO can sweep it) and
outputs only the delayed signal; an echo is a Mix, the Delay and a Gain wired
round in a loop (`examples/feedback-echo.ron`). Every lane has its own line,
and a lane silent for a whole line's length is flagged silent.

## Library and presets (M9)

Terms are in `CONTEXT.md`; the decisions are ADRs 0004 and 0005.
`noodle_core::library` holds the types, `Session` the loaded library.

- **Entries.** A `GroupTemplate` (a group and its contents), a `NodePreset`
  (one node's parameters and config) and a `GroupPreset` (every node inside a
  group). A library file is one entry as RON (`Item`); the embedded system
  files are lists of entries (a template and its presets).
- **Stores.** `Library` has the system entries, embedded with `include_str!`
  from `noodle-core/library/system/` and read-only, and the user entries, one
  `.ron` file each in the user data dir (`<data dir>/library`, found by
  `prefs::library_dir`; copy a file to share it). With no directory it keeps
  user entries in memory, which tests use. IDs are `store:kind:slug`, e.g.
  `system:template:kick`, `user:group:warm-pad`. There is no project-level
  store. Unreadable user files are listed in `Library::problems`, not fatal.
- **Templates are copies.** `GroupTemplate::from_group` keeps the group,
  everything inside it (any depth), the wires between them and the nodes'
  positions relative to the group; it drops wires to the outside, automation
  lanes, the group's own boundary gain, and mute and solo on every boundary
  node (a nested group's gain is its level inside, so it stays, as in a
  group preset). `instantiate` returns one
  `Command::Batch` that adds an unlinked copy under fresh IDs (the group, then
  the layers inside it in the template's ID order) and sets `Node::template`
  to the entry ID. Nothing ever follows that link; it only says where the
  copy came from.
- **Tracks are a hidden template.** `system:template:track` has
  `hidden: true`: `Library::templates` doesn't list it and the Add menu
  doesn't offer it. `group::create_track` instantiates it, and the Add track
  button (and every test that makes a track) goes through that.
- **Presets set stored values only.** Applying one is a `Command::Batch` of
  `SetParam`, `SetConfig` and `SetPreset`: one undo step, and it never
  touches wires, lanes, mute or solo. Values the preset doesn't store go back
  to their defaults. `SetPreset` records the entry in `Node::preset` (no
  effect on the engine); the Presets control shows the entry's name, and
  "modified" once the node's values differ from it. Config named in
  `presets::KEPT` (the Sampler's `sample`, the Envelope's `single`) is left alone when the preset
  doesn't store it, so applying a preset never unloads a sample or changes a voice shape.
- **System node presets** live in one generated file, `library/system/nodes.ron`
  (a table in `noodle-eval/src/song/node_presets.rs`, rewritten by
  `UPDATE_LIBRARY=1`). Every node has at least two, except those with nothing
  to set; a test lists the exceptions. Dynamics has Compressor, Vocal
  Compressor, Bus Glue, Limiter, Gate, Drum Gate, Expander and De-noise.
- **Presets find ports by name.** Port layout comes from the node types,
  which `noodle-core` doesn't know, so the functions that need it take a
  closure; the app passes `Registry::layout_signature` (the sorted keys of
  every port, no positions). A node preset fits a node of the same type that
  has every parameter it sets, and whose ports its config wouldn't change; so
  a node type gaining or reordering ports doesn't break its presets.
- **Topology** (`library::topology`) decides which groups a `GroupPreset`
  fits: the same node types, wires and boundary port names (a group preset also
  needs each node to have the parameters it sets). Config
  that doesn't shape ports (a sample, an expression) doesn't count, nor do
  IDs, labels or positions. Nodes are put in a canonical order by colour
  refinement (each node starts from its type and boundary name and
  takes on its neighbours' colours until stable); nodes still
  indistinguishable (the five saws of a unison) fall back to ID order, so a
  copy of a template pairs them as the template did. A preset stores its
  values in that order with the topology key (FNV-1a, fixed so keys in saved
  files stay valid).
- **System templates** are the song kit's instruments (`Mono Synth`,
  `Poly Synth`, `Pad`, `Kick`, `Snare`, `Hat`, `Clap`), the subtractive
  example's `Subtractive Voice`, the tempo-synced `Tempo Echo` and `Tempo
  Tremolo` (Tempo into a Delay or an LFO) and an empty `Drum Rack` (a Note
  Split in front of eight Samplers on General MIDI keys, samples picked per slot,
  ADR 0009), each with three or more group presets. They are
  generated by `noodle-eval/src/song/system.rs` (so they are groups of
  primitive nodes, editable once added) and a test keeps the files in step:
  `UPDATE_LIBRARY=1 cargo test -p noodle-eval --test library`.
- **UI.** The properties panel's header has a Presets control (System and
  User sections, Save to overwrite the user preset applied, Save as...); the
  Add menu (Shift+A) lists templates under Groups; right-click a group for
  "Save group to library...". `Edit::Library` saves to the user store and is
  not an undo step.
- **Update from template.** Right-click a group made from a library template
  (its `Node::template` names an entry the library still lists) for "Update
  from <name>". `GroupTemplate::update_group` is one `Command::Batch`, one
  undo step: it removes everything inside the group and adds the template's
  current contents (new IDs, positions relative to the group). The group node
  stays, with its ID, place, label, name and the wires at its ports. Wires at
  a port the template no longer has are removed, the group's own gain, mute
  and solo are carried over by port name, and the applied preset is cleared.
  Edits inside, and the automation lanes on nodes inside (including lanes on the group's own gain and mute, which sit on its boundary nodes), are lost; the status
  bar says how many lanes. It is always an explicit choice (ADR 0005); nothing
  ever updates a group by itself. The hidden track template can't be applied
  this way.
- **Not yet.** User track templates.

## Instruments, dynamics and effects (M9)

These are primitives; instruments and racks built from them are groups.

- **Sampler** (`noodle.instrument.sampler`) plays one sample per note from a
  note-event input, up to 16 voices mixed to a stereo output. The sample is a
  config setting, the blake3 hash of the decoded audio (ADR 0004), resolved at
  instantiation through the `SampleStore` that `register_library` returns: the
  store keeps audio in memory and reads `<hash>.wav` from a folder, which the
  app, the CLI and every offline render set to `samples` beside the project
  file. A sample the store doesn't have is silence, and makes the node
  uncacheable, so a render of silence is never remembered. Parameters: `root`,
  `tune` (cents), `keytrack`, `start`, `end`, `mode` (one-shot or gate), `loop`
  and loop points (gate mode only), `release`, `gain`. Interpolation is
  four-point Catmull-Rom.
- **Note Split** (`noodle.event.note_split`) routes notes to `outputs` event
  outputs (config, 1 to 16) by key range and velocity range, with `transpose`
  per output and an optional `rest` output (config) for what nothing took.
  A note goes to every output that matches (layering); note-offs and
  expression follow their note-on; other MIDI goes to all outputs. One output
  and no rest is a note filter.
- **Dynamics** (`noodle.effect.dynamics`) is a compressor, limiter, gate or
  expander by `mode`, with a `detect` choice of input or sidechain, a
  gain-reduction output, and gain computed in the log domain with a soft knee.
  `lookahead` (0 to 20 ms) is config, not a parameter, because the plan fixes
  a node's latency before audio runs; it delays the audio and reports itself
  to delay compensation. Channels share one gain. Compressor, Limiter, Gate
  and Expander (and variants such as Vocal Compressor and Drum Gate) are
  system *presets* of this one node (`library/system/nodes.ron`), not
  groups.
- **Parametric EQ** (`noodle.effect.eq`) is `bands` (config, 1 to 8) RBJ
  biquads in series: bell, shelves, cuts and notch. Coefficients are computed
  once a block when nothing is wired and every 32 samples when something is.
- **Reverb** (`noodle.effect.reverb`) is a stereo feedback delay network of
  eight lines with a Householder matrix, input allpasses and a slow length
  wobble. One-voice input, stereo output, deterministic.
- **Arpeggiator** (`noodle.event.arpeggiator`) turns held notes into steps on
  the transport's tick grid (free-running while stopped): up, down, up-down,
  down-up, as played or random (seeded by the node's seed), over 1 to 4
  octaves, with gate, swing and hold.

- **Envelope** (`noodle.mod.envelope`) plays a drawn curve over `length` (or,
  with `time` set to Beats, over `beats` beats at the transport's tempo, read
  at each block) when its `trigger` rises through 0.5 (a new trigger restarts it; `loop` repeats it
  while the trigger is held). The curve is config text, `x y bend` per anchor:
  the ends sit at x = 0 and 1 and can be at any height, and each segment bends
  by `t^(4^bend)`. It is a function of x, so the node plays it from a lookup
  table built when the node is built (`EnvelopeCurve::lut`), like Remap's.
  `single` is config because it changes the output's shape: 0 gives one
  envelope per voice of the trigger, 1 one shared mono envelope that any
  voice's trigger restarts. Outputs `out` and `active` (1 while playing). An
  edit to the curve rebuilds the node and restarts a running envelope. The
  node's body in the editor draws the curve.
- **Automation** (`noodle.mod.automation`) is the node behind a free
  automation track: one parameter, `value` (0 to 1), copied to `out`. The
  timeline's "+ Auto" button adds it with an ordinary lane on `value`, so
  nothing new compiles: the lane is wired into `value` as always, and `out`
  is wired by hand to any number of inputs. Its lane draws under the track
  whose group holds the node, or, for a node outside any track's group, in
  its own row after the last track (`Rows::free_lanes`).
- **Effects and modulators added in M9 batch 4.** Distortion
  (`noodle.effect.distortion`: soft, hard, fold, asymmetric), Bitcrusher
  (`noodle.effect.bitcrusher`), Chorus (`noodle.effect.chorus`, a modulated
  delay with feedback, so also a flanger; a second channel sweeps a quarter
  cycle behind) and Phaser (`noodle.effect.phaser`, 2 to 12 allpass stages)
  are per-lane effects; Chorus owns its delay lines like Delay, and neither
  it nor the others allocate or lock on the audio thread. Envelope Follower
  (`noodle.mod.follower`, peak or RMS), Sample & Hold
  (`noodle.mod.samplehold`, sample or track mode), Lag
  (`noodle.mod.lag`, separate rise and fall times; the first sample is taken
  as is) and Comparator (`noodle.mod.comparator`, a Schmitt trigger) turn
  signals into control signals and gates. Clock (`noodle.generator.clock`)
  pulses on a note division of the transport's tick grid (silent while
  stopped), so it stays in time through tempo changes, seeks and loops, to the resolution of a block (a tempo change or loop wrap inside a block lands at the next).
  Tempo (`noodle.mod.tempo`, ADR 0008) turns a division of the beat into `time`
  (seconds) and `rate` (hertz) outputs that follow the transport's tempo, using
  Clock's division table (which also has dotted values and 2 and 4 bars). A
  wire into an offset slider such as Delay's `time` or the LFO's `rate` moves it
  along its travel instead of setting it, so the synced templates put a Math node
  between (seconds divided by Delay's 2 s range, hertz as a log fraction of the
  LFO's range) and leave the slider at its minimum.
  Beat Length (`noodle.mod.beat_length`) is the same for any tempo: a `bpm`
  parameter (wire a tempo into it; a wire sets its value) and a `division`, with
  the same `time` and `rate` outputs.
