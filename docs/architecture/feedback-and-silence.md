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
- **Loop regions.** The compiler collects every node on a loop (a strongly
  connected component of the wires that are left once refused loops are
  dropped, with the breaker's two halves together) into a *loop region*. Two
  loops that share a node are one region. `sort` orders a region as one unit, so
  its steps are consecutive in the schedule (the order inside it is the usual
  topological one), and `allocate` gives back the buffers a region's steps used
  only when the region is done, so no two of its steps share a buffer.
  `Schedule::regions` names the ranges. Nodes outside a region are untouched
  and run a block at a time.
- **Chunk size.** Before it runs each block, the plan asks every loop-breaking
  node in a region `Node::loop_chunk(ctx, inputs, held)` with the node's
  signal inputs for the whole block, and takes the smallest answer (clamped to
  the block). The default is the whole block, which is how a loop ran before
  regions (`Voices`' `busy` loop is unchanged). `Delay` answers the floor of
  its shortest `time` in the block, in samples, at least 1: a chunk then reads
  only samples earlier chunks wrote. A signal the breaker reads from a node of
  the region itself (a second loop's Delay setting this one's `time`) isn't
  written yet when the chunk is asked for, so the plan plays safe and answers 1
  for it. The Feedback node answers 1. A loop with a 40-sample Delay
  therefore runs its nodes 40 frames at a time; a 300 ms echo has a chunk longer
  than the block and runs exactly as before.
- **Running a region** (`plan::run_region`). The region's first step runs every
  member, the others do nothing (in the step graph they wait for the first,
  which waits for everything any member waits for, so on several threads a
  region is one step). With a chunk shorter than the block, each live member
  runs for each chunk in order on windows of its inputs, copied into chunk
  buffers the plan owns (`Chunk`, sized when the plan is built) and copies what
  it wrote back into its block buffers, where nodes after it, and nodes outside
  the region, read it. The context carries the chunk's length and a transport
  advanced to the chunk's start. Nothing allocates on the audio thread.
  - *Events* in are windowed per chunk with their offsets rebased, and
    events out are rebased back and appended.
  - *Silence flags.* A node in the region that reads another sees that node's
    flags for the chunk (a consumer's skipping depends on them, so it must see
    what a block of that length would). A reader outside sees the AND over the
    chunks. A lane is flagged silent for the block only if it was in every chunk.
  - *Wired parameters* are worked out per chunk from the window of the wire.
  - *Meters, probes and the state report* are made once per block, after the
    region, from the block buffers; a node's CPU time is the sum over its
    chunks.
  - *Panics* on a worker are caught around the whole region (every member is
    silenced for the block).
- **Held regions.** A region with a hosted plugin in it
  (`NodeType::chunkable` false), a spectral node, or a node with a delay
  compensation line in front of it runs whole blocks, as every loop did before
  regions. `Delay` is then held to at least `max_frames` (the engine's largest
  block, so the delay doesn't change with the block in hand), and the node's
  `NodeActivity::held` says so; the Latency section of the properties panel
  explains it.
- **Cost of a loop.** A node in a region is called once per chunk, which costs
  about half a microsecond per call besides the node's own work, so a node in a
  loop running sample by sample costs about 2 to 3% of a core at 48 kHz, and
  proportionally less at longer chunks. A node processes all its voices in one
  call, so polyphony does not multiply the calls, though every voice runs at
  the chunk of the shortest delay among them. The properties panel shows the
  chunk a node last ran at ("runs 12 samples at a time, in a feedback loop"),
  and the CPU meters already show the cost. There is no setting. A loop's sound
  no longer depends on the engine's block size.
- **The Feedback node** (`noodle.util.feedback`) is a one-sample delay that is
  a loop breaker, with chunk 1. It makes delay-free loops legal and explicit:
  feedback FM (an oscillator's output into its own frequency), a filter built
  from integrators. Without it such a wire still gets the `Loop` diagnostic,
  whose text suggests the node. Outside a loop it is a plain one-sample
  delay. Not in `Delay`'s class: it needs a region (or a held block, where it
  delays a block).
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
