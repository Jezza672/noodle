# Feedback loops and silence skipping

A wire can close a loop only through a node that does not need its input to
write its output. A node type declares that with `NodeType::loop_input`, the
key of the signal input it reads *last*: `Delay`'s `in`, and `Voices`' `busy`.

- **Two steps.** A loop-breaking node that lies on a loop runs as two steps in
  the schedule (`Phase::Output`, then `Phase::Input`): the output half writes
  every output from state it already has (and reads every other input, and
  the event inputs), and the input half, later in the block, reads the loop
  input and updates state. The node implements `Node::process_output` and
  `Node::process_input` for this, and `Node::process` for when it is not on a
  loop (it must be equivalent to the two in a row). In the compiler the
  node becomes two vertices of the dependency graph, the output half
  ahead of everything downstream and the input half behind everything
  upstream, so the loop becomes a chain. Both halves belong to one plan slot
  and one instance; the input half holds no instance of its own.
- **What stays refused.** A loop that reaches the node through any other
  input (a `time` wire, say) or through nodes that are not loop-breakers
  gets the usual `Loop` diagnostic on the wire that closes it.
- **Cost of a loop.** The output half runs before this block's input exists.
  `Delay` therefore reads at least `max_frames` samples back while in a loop
  (the engine's largest block, a constant, so the delay doesn't change with
  the size of the block in hand; about 2.7 ms at the app's 128 frames); shorter times are held to that. A `Delay` that
  is not on a loop has no minimum.
- **Shapes.** The output half is shaped before the loop input is known. The
  compiler assumes a mono loop input, finishes, and if the real input
  differs, starts shape inference again with that assumption, up to four
  passes; a loop that never settles reports a node diagnostic and the node
  does not run.
- **Buffers.** Nothing special: the output half's buffers are allocated at
  its step like any other output, and the input half reads its input buffer
  at its own, later step, so the usual lifetime rules hold.

## Silence skipping

Every lane (one voice of one channel) of a signal carries a *silent* flag:
"this block is exactly zero". Nodes that know it set it (`SignalOut::set_silent`,
or `silence`, which also writes the zeros) and nodes downstream read it
(`SignalIn::is_silent`). A lane is also silent when its whole signal is the
constant 0, such as an unconnected audio input. The flags are cleared when a
node's outputs are handed to it each block, so a node that says nothing is
never claimed silent; they live in a plan-wide array parallel to the buffer
pool, so nothing allocates.

- **The wrapper does it.** A `LaneKernel` opts in with `skip()`:
  `Skip::AnySilent(ports)` for a multiplier or a generator driven by a
  pitch, `Skip::AllSilent(ports)` for a mixer. `PerLane` then skips a lane
  whose inputs are silent *and* whose `is_idle(state)` is true, writing zeros
  to every output and flagging them silent, so the saving cascades. Without
  `is_idle` a filter's tail would be cut off: a lane that is still ringing runs
  until its state has decayed (SVF and ladder below about -140 dB, an ADSR
  that has finished its release). Generators that make sound from silent
  inputs (noise, anything with a reverb tail) leave `skip()` at `Never`.
  The oscillators skip a lane whose *frequency* is silent, since a voice with
  no pitch is not sounding; this differs from a true 0 Hz, which would hold
  a constant, but no parameter range reaches 0.
- **Where it starts.** `Voices` flags a voice's `gate` silent for any block
  in which it stays low, and flags all three of its outputs silent (writing 0
  for the pitch) for a voice that has finished (see below) or never been
  used. Then the envelope goes idle and flags its output, the VCA and the filter
  skip the lane, and `Voice Mix` adds only voices that sound.
- **When a voice is finished.** Either the envelope says so or the `tail`
  does. Wire the ADSR's `active` output (1 while the gate is high or the
  release is still going, polyphonic like the envelope) into `Voices`'s `busy`
  input: a voice that has been busy since its note began goes inactive the
  block its envelope finishes, however long the release was. That wire is a
  feedback loop (the envelope follows the voice), allowed by the loop rules
  below. Without a `busy` wire a voice waits `tail` seconds, which should be
  at least the longest release in the patch; and `tail` also caps a `busy` that
  never falls. A voice stops sounding when it goes inactive.
- **A skipped oscillator doesn't run its phase.** A note on a voice that was
  inactive therefore starts the oscillator at phase 0 rather than wherever it
  free-ran to.
- **Real-time rules** hold: a skip is a branch and a memset.
- **Plugins join silence skipping.** A hosted plugin that has said it is done
  (CLAP `Sleep`, VST3 output `silenceFlags`, AU `OutputIsSilence`) while its
  inputs are silent and nothing woke it is not called; its outputs are flagged
  silent. It is also a node like any other for liveness: it runs only while its
  outputs reach an Output. See "The bridge" in [plugins.md](plugins.md).
- **Nodes nobody listens to don't run.** At the start of each block the plan
  works out which nodes are live (`Plan::update_liveness`): those that always
  run (Output nodes, taps, and types whose `NodeType::always_runs` is true:
  scope, meter, MIDI In, track input, metronome) and every node whose
  outputs a live node reads. A node whose outputs are unconnected, or are read
  only by nodes that are *silenced* (`Node::silenced_by`: a stage with its
  mute settled on, a VCA or multiplier whose level is a constant 0), stops, and
  so does everything that only feeds it, back to the sources. A silenced
  node's other inputs may come from a source whose output is known without
  running it (`Node::constant_output`, used by the track input's mute). A stopped node whose outputs a live node still reads has them
  zeroed. When a node becomes live again it is `reset()` and its delay lines
  cleared, so it starts fresh rather than playing stale state.
