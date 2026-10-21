# Signals

The kinds of signal are audio, events, spectral frames and a wire that carries a
reference to a sample instead of signal:

- **Audio:** f32 samples shaped `[voices][channels][frames]`. Normal signals
  have `voices = 1`. Shapes **broadcast** the way arrays do in numpy: in each
  dimension the sizes must match or one of them must be 1. A node runs at
  the broadcast shape of its inputs, so a mono LFO modulates all 8 voices of
  a polyphonic filter without anything special being done. A *Voice Mix* node
  sums voices back down to 1. Audio-rate modulation is just audio, so there's
  no separate CV type.
- **Events:** a per-block list of timestamped events (sample offset within the
  block, note ID, payload) covering notes, note expressions and MIDI. A
  *Voices* node turns events into polyphonic pitch, gate and velocity signals,
  doing voice allocation and stealing.
  - **In the graph:** event ports are their own kind (`Layout::event_input`
    and `event_output`, drawn as diamonds in a different colour). An event
    wire only joins event ports, the compiler gives each event output a
    buffer with room for 1024 events a block (more are dropped, and a node
    that pushes a note-on it can't fit must not count the note as started),
    and an unconnected event input is an empty list. Events don't broadcast
    over voices; they carry their own note IDs.
  - **Nodes so far:** `Key` (`noodle.event.key`) makes notes from a gate
    parameter and a key number, so a Button or any signal can play a note.
    `Mono Note` (`noodle.event.mono`) plays the latest held note as `pitch`
    (Hz), `gate` and `velocity` signals, falling back to an earlier held note
    on release and holding its pitch after the last so an envelope's release
    stays in tune. The track input's `midi` output is also an events port.
    `MIDI Control` (`noodle.event.control`) turns a controller (0 to 127),
    pitch bend (128) or channel pressure (129) on a channel (0 = any) into a
    signal (0 to 1, pitch bend -1 to 1) with a `glide`; MIDI 2.0 values arrive
    at 32 bits (as far as an `f32` signal holds them), a MIDI 1.0 controller
    pair (n and n+32) at 14 bits, a single controller at 7.
    `Voices` (`noodle.poly.voices`) plays many notes at once: its `voices`
    config (1 to 64, default 8; config because it is the outputs' shape) sets
    the lanes of its `pitch`, `gate` and `velocity` outputs, one voice per
    note. A new note takes the free voice that has been free longest, so
    releases ring out; with none free it steals the voice whose note started
    first. A steal fades: the voice keeps its old note while the `fade`
    output (a fourth output, a gain to multiply in after the envelope) ramps
    down over `steal_fade` seconds (a parameter, default 5 ms), then switches
    to the new note with the gate dropping for a sample so envelopes
    retrigger, as `fade` ramps back up. The new note therefore starts
    `steal_fade` late. At 0 the steal is hard: the pitch changes at once,
    which can click on a bright patch. A
    note-on for a key already held retakes its voice. Pitch
    expressions bend only their own note's voice, and a pitch holds after
    release until the voice goes inactive, either when its envelope reports
    it is done (the `busy` input, a feedback loop) or after `tail` seconds (a
    parameter, default 10): see "Silence skipping" below.
    `Voice Mix` (`noodle.poly.voice_mix`) sums voices.
    `MIDI In` (`noodle.event.midi_in`) plays the app's MIDI input port: see
    "MIDI input" below.
  - **Testing:** `Harness::send_events` and `Harness::events` feed and read a
    node's event ports.
- **Spectral:** FFT frames, for processing a sound by its frequencies. Every
  `HOP` (512) samples a spectral wire carries one frame per lane: `BINS`
  (1025) complex bins from 0 Hz to Nyquist, an `FFT_SIZE` (2048) Hann window
  of the last samples (`noodle_engine::spectral`). The frame size is the same
  for the whole engine, which is what lets any spectral node feed any other
  (decision in ADR 0007). Bins are scaled so a sine of amplitude `a` has
  magnitude `a` on its bin.
  - **In the graph:** spectral ports are their own kind (`Layout::spectral_input`
    and `spectral_output`, drawn as violet squares and wires, and
    `#[spectral_input]`/`#[spectral_output]` in `#[derive(Ports)]`). A spectral
    wire only joins spectral ports (the compiler reports `KindMismatch`
    otherwise). A node's spectral ports have a `Shape` (voices x channels,
    one lane each) like audio: `NodeType::output_shapes` is given the signal
    input shapes followed by the spectral input shapes and returns the signal
    output shapes followed by the spectral output shapes, so the default gives
    an Analyze the shape of its audio and a Synthesize the shape of its
    spectrum. `Setup::input_shapes`/`output_shapes` carry both too. An
    unconnected spectral input is an empty list of frames.
  - **Frames in a block** are timestamped like events: `time` is the offset
    within the block of the last sample the frame covers. A block holds
    `max_frames / HOP + 2` frames at most (the buffer is sized once per plan,
    like the signal pool, so nothing allocates on the audio thread), so a
    4096-sample block holds eight and a 64-sample block usually none. A spectral
    node is written like an event node: for each frame in, push a frame out at
    the same time (`SpectralIn`, `SpectralOut`). Unlike events, a spectral
    buffer is `lanes x BINS` bins per frame, and buffers are reused after their
    last reader like signal buffers.
  - **Analyze and Synthesize** (`noodle.spectral.analyze`,
    `noodle.spectral.synthesize`) wrap `Analyzer` and `Synthesizer`. Analysis
    adds no delay; synthesis (overlap-add of Hann-windowed inverse transforms)
    sounds `SYNTH_LATENCY` (2047) samples late and reports it through
    `Node::latency`, so plugin delay compensation lines up a dry path mixed in
    beside the pair and offline renders and exports drop it. The pair gives the
    input back to within float error at any block size
    (`tests/spectral.rs`). Lateness travels along spectral wires, so a latent
    plugin ahead of an Analyze, or two pairs in a row, are compensated for in
    the dry paths beside them. Spectral inputs are compensated like signal
    inputs (see "Delay compensation"): where spectral paths of different
    lateness meet (a Cross), the earlier one's frames are held back by a
    `SpectralDelay` so both carry the same moment.
  - **Spectral nodes** (category Spectral, each frame in, one frame out, no
    delay of their own): `Spectral Gate` (bins under a threshold turned down,
    with a knee), `Spectral Filter` (band pass or stop between two
    frequencies), `Spectral Shift` (every partial moved by the same number of
    hertz, rounded to whole bins; the frames are turned by a phase that
    advances `k` bins' worth per frame so the partials stay coherent),
    `Spectral Freeze` (holds each bin's level and keeps advancing its phase at
    the frequency it was caught at), `Spectral Blur` (smooths levels over time,
    keeping the live phases) and `Cross` (the levels of a `modulator` spectrum
    on a `carrier`, blended with `mix`; the modulator's latest frame is used
    for each carrier frame, so the two analysers need not have started
    together).
  - **Caching:** a node's cache key covers its spectral inputs like its event
    inputs, so a freeze across a spectral chain keys correctly. Offline nodes
    take no spectral input.
  - **Testing:** `Harness::with_spectral`, `spectral_input` and `spectral`
    feed and read a node's spectral ports; `tests/spectral.rs` runs whole
    graphs, `realtime.rs` checks chains for allocation.
- **Sample:** a reference to a sample, not signal: a content hash (ADR 0004)
  and optionally a window of it, as fractions (`SampleRef`, `hash` or
  `hash:start:end`). Sample ports are `Layout::sample_inputs` and
  `sample_outputs` (`#[sample_input]` and `#[sample_output]` in `Ports`,
  drawn as orange triangles). They have no buffers and no place in the shapes:
  the compiler resolves them before it schedules anything (`resolve_samples` in
  `compile.rs`), by asking the source node type for what its output carries
  (`NodeType::sample_output`) and writing that text into the receiving node's
  config under the input's key, replacing what was there, then dropping the
  wire. So a wired sample overrides the option it stands for, a group's
  boundary needs nothing special (groups are flattened first, and a boundary
  port takes any kind), and the config feeds the node's cache key like any
  other. A wire from a node with no sample sets the option empty (silence).
  `Sample` (`noodle.instrument.sample`) holds a `SampleRef` as its `sample`
  option and outputs it; the Sampler's `sample` is its input, and a window
  plays as if it were the whole sample.
