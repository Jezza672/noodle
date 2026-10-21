# M3: Events and polyphony

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

It becomes an instrument.

**Status:** batch 5 is done: the `Delay` node and feedback loops (the
compiler runs a loop-breaking node in two steps, see "Feedback loops" in
the docs/architecture/ files (index: ../ARCHITECTURE.md)), `Voices` taking a `busy` input from the ADSR's new `active`
output so a voice is retired when its envelope ends, and a fade before a voice
steal (`steal_fade`, with a `fade` gain output). `examples/feedback-echo.ron`
is new, and `examples/poly-synth.ron` and `examples/subtractive-synth.ron` use
the new wires. The modulation backlog (log versus linear taper, the live meter
with min and max ticks) had already landed in batch 1. M3's code is complete;
what is owed is a real-hardware MIDI check and a listening test on the Mac.
Batch 4 is done: silence skipping and finished-voice skipping
(per-lane silence flags, `Skip` and `is_idle` on `LaneKernel`, a `tail` on
`Voices`; see "Silence skipping" in the docs/architecture/ files (index: ../ARCHITECTURE.md)), and the rest of the
synthesis nodes: `Square` and `Triangle` band-limited oscillators, a `Ladder`
filter, a `Pan` and a `Math` node (arithmetic on four inputs from a typed
expression), with `examples/subtractive-synth.ron` (the synth
is a group of those primitives, unison included as a nested group, with its
controls as group inputs) and its
golden render. Batch 3 is done: the `Voices` node (voice allocation and
stealing) and polyphonic signals end to end with `Voice Mix`, played from a
MIDI keyboard and a MIDI clip (`examples/poly-synth.ron` and its golden
render); the ADSR now recomputes only its moving times, and offsetting wires
convert held lanes once. (Stealing was hard until batch 5, which fades.)
Batch 2 is done: MIDI input (a port chosen in the audio settings
dialog, the `MIDI In` node), MIDI clips on tracks, and a piano roll (see "MIDI
input" and "MIDI clips" in the docs/architecture/ files (index: ../ARCHITECTURE.md)), with `examples/midi-clip.ron` and
its golden render. Batch 1 is done: the port derive macro (`noodle-macros`), events
as a signal type with the `Key` and `Mono Note` nodes, the ADSR, LFO and VCA
nodes, a general-purpose Remap node (input range to output range along a
Bézier curve drawn in the properties panel, `examples/remap-sweep.ron`), and the modulation display (offset-in-slider-space parameters, and a
live meter with min and max ticks on wired parameters), with a monophonic
synth example (`examples/synth-pluck.ron`) and its golden render. Still to
do, in order: silence skipping and finished-voice skipping; the
rest of the synthesis nodes (band-limited oscillators, SVF and ladder
filters, unison and spread); the delay node and feedback loops.

- **Port derive macro, before writing the synthesis nodes.** *Done.*
  - **The problem:** nodes refer to ports by position
    (`const CUTOFF: usize = 1`), and if those constants drift out of step with
    the layout, the node silently reads the wrong input.
  - **The fix:** a derive macro generates the `Layout` and named accessors
    from one declaration.
  - **Why here:** M1 and M2 will have settled the node API through real use,
    and M3 roughly triples the number of nodes.
  - Nodes whose ports depend on config, such as Mix and plugin nodes, keep
    using the `Layout` builder.
- Events signal type. *Done* (the engine already carried them; batch 1 added
  the `Key` and `Mono Note` nodes, test-harness support and an end-to-end
  patch).
- MIDI input (midir), MIDI clips and a piano roll. *Done* (batch 2).
  - Live MIDI is timestamped inside the block and the app plays 128-frame
    blocks; the computer keyboard is a MIDI source (toolbar button); Record
    captures MIDI into a clip with an optional quantise grid and
    output-latency compensation.
  - The 128-frame block is a fixed choice for now (4× the per-block overhead
    of 512); the engine block could follow the device period again.
  - **Still to do:** quantise and copy/paste in the piano roll, more than one
    MIDI port at a time, overdubbing into an existing clip, a count-in,
    keeping the raw take for re-quantising. (Reopening a port that was unplugged and comes back is
    done: the session looks at the port list about once a second.)
- Voices node: voice allocation and stealing, producing polyphonic pitch, gate
  and velocity. *Done* (batch 3).
- Polyphonic signals working end to end, and the Voice Mix node. *Done*
  (batch 3).
- **Silence skipping.** *Done* (batch 4).
  - **Flags:** signals carry a per-lane silence flag, which generalises the
    current per-signal `constant` flag.
  - **The wrapper:** a reusable lane-kernel wrapper skips a lane while all its
    audio inputs are silent, and marks that lane of its output silent, so
    the saving cascades downstream.
  - **Opt-in:** generators such as oscillators make sound from silent
    inputs, so they don't use the wrapper.
  - **Tails:** a lane is only skipped once its output has also gone silent,
    so filter and reverb tails ring out.
- **Skipping finished voices.** *Done* (batch 4, as a `tail` parameter; batch
  5 added the envelope report). `Voices` marks a voice inactive once it has
  been free for `tail` seconds, or as soon as its envelope's `active` output,
  wired into `Voices`' `busy` input, falls; nothing processes an inactive
  voice until it's reused.
- **Fade before a steal.** *Done* (batch 5).
- **Synthesis nodes.** *Done* (batch 4; unison is a group of saws, `Math` detune and `Pan`):
  - Band-limited oscillators.
  - ADSR envelope and LFO.
  - SVF and ladder filters.
  - VCA, unison and spread.
- **Delay node and feedback loops.** *Done* (batch 5).
  - **Before:** the compiler dropped any wire that closed a loop, and showed a
    diagnostic on it.
  - **The change:** a node type declares the input it reads last
    (`NodeType::loop_input`). The compiler then allows loops that pass through
    such a node, running it in two steps when it is on a loop. Loops through
    anything else are still dropped. `Delay` is the first such node, and
    `Voices` (its `busy` input) the second.

**Design rule (Jeremy):** a synth is never a monolithic node. It is a group
built from primitive nodes, with its controls as the group's inputs, editable
after it is made. Add primitives (oscillators, filters, math), not synths.

**Done when:** you can build a polyphonic subtractive synth from nodes and
play it from a MIDI keyboard and from a MIDI clip.

## Live playing without the transport (done)

Requested by Jeremy: play a graph from a MIDI instrument without starting the
timeline. The design is in [ADR 0001](../adr/0001-engine-runs-independently-of-the-transport.md)
and "The engine and the transport" in the docs/architecture/ files (index: ../ARCHITECTURE.md). Done in one batch: the
engine starts with the app and the toolbar's Audio toggle turns it off; Play,
Stop and rewind only move the transport; an armed track plays live MIDI
through its own synth (live monitoring); and a failed output falls back to
the system default and then to the new virtual output, which can also be
picked in the audio settings as "Virtual (no sound)". Not done: remembering
the toggle between launches, and retrying the saved device when it returns.

## MIDI clip looping (done)

MIDI clips can loop (#196): `loop_on`, `loop_start` and `loop_len` on
`MidiClip`, a Loop checkbox with start and length in the piano roll's toolbar,
and a draggable loop brace on its ruler. The playback rules are in
[midi-and-recording](../architecture/midi-and-recording.md). Tests:
`noodle-core` `clip.rs` (how the notes repeat), `track_input_midi.rs` (the
events), `piano_roll/tests.rs` (the controls and brace).
