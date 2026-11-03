# Selector

PR #388. A deterministic N-way selector for audio and control signals: one
output routed from a chosen input by a parameter, where Random Select picks at
random on a trigger.

- **The node.** `noodle.util.selector`, "Selector", in Utilities. One `out`,
  the audio and control inputs `in1`…`inN`, and the stepped parameter `which`
  with the labels `In 1`…`In N`.
- **Config and parameters.** How many inputs (`inputs`, 2 to 64, default 2) is
  config, because it changes the ports. `which` is a parameter, numbered from
  0 internally, so it can be automated or wired; the node rounds it and clamps
  it to an input, and a wired value chooses per sample.
- **Shapes.** The output takes the broadcast shape of the inputs, so a mono
  input plays on every voice and channel another input sets. An input with
  nothing wired is silent, so a half-patched selector still passes whichever
  input is selected.
- **Memoryless and real-time.** `Memory::None`, a lane kernel in `PerLane`,
  `Skip::AllSilentBelow(inputs)`, and no allocation.
- **Use.** A send is one wire: a Gain's input is the pre-fader tap and its
  output the post-fader tap, so a Selector on those two taps makes pre/post
  switching a `which` change rather than moving a wire. S19 does not depend on
  it; its route still wires the taps out to a second Mix (#332).
- **Not built.** Switching is instant, so switching between unrelated signals
  can click; there is no zero-crossing option. Notes, samples and spectral
  frames are not accepted; the kinds are a roadmap item.

Tests: `selector.rs` (selection, rounding and clamping, a wired `which`,
broadcasting, an unconnected selection, the default layout and bad input
counts), `realtime.rs`
(`selector_never_allocates_through_switches_wiring_and_resizes`), and
`reset_matches_fresh.rs`, which runs every registered type.
