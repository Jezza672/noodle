# Tempo sync and the drum rack

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** done (ADRs 0008 and 0009). Closes #142 and #143, which turned out to be half
built already: the Sampler and Note Split came with M9 batch B, and the songs'
drums are a rack of them.

1. **Tempo node.** `noodle.mod.tempo`: a `division` choice, and `time` (seconds)
   and `rate` (hertz) outputs that follow the transport's tempo. Allocation
   test, stateless, tests at two tempos.
2. **Library templates.** `Tempo Echo` (Tempo, Lag, Delay with feedback, wet mix),
   `Tempo Tremolo` (Tempo into an LFO into a VCA) and an empty-slot `Drum Rack`,
   each with `division` or per-slot controls as group inputs, generated with the
   other system templates.
3. **Not in scope.** The warp evaluation songs still leave echo and LFOs out, so
   their baselines do not change; putting a synced echo in them is a decision for
   the evaluation, not for this batch.

## Node editor polish (#204, #206, #207, #225)

- **Group output meters.** A group's output ports always keep their
  `noodle.group.stage` in the compiled graph (and so in a freeze's cache), and
  the stage reports its output level like Gain does. The group node and its
  output node draw one bar per channel of every output port.
- **Hover highlight.** The node under the pointer gets a lighter outline; the
  other nodes' fields no longer draw in the disabled colours.
- **Note names.** `Unit::Note` shows a MIDI note number with its name (`60 C4`)
  and accepts names (`C4`, `f#3`, `Db2`) when typed. Note Split's `low` and
  `high` use it.
- **Remove unused ports (#225).** Ctrl+double-click a node, or right-click it,
  to drop its unused ports in one undo step (`spare::remove_unused_ports`): a
  Mixer's inputs past the last wired one, and a group's (or its input or output
  node's) ports with no wire outside or inside. Other nodes are unaffected.

## Drum pads and node names

The Drum Rack template's slots are `Drum Pad` groups named for their drum (Kick, Snare, Closed Hat, Open Hat, Clap, Low Tom, High Tom, Crash). A pad owns its key (a one-output Note Split, set by the `key` input), a Choke Gate, a Sampler and a Pan, with `notes`, `key`, `level`, `tune`, `pan` and `choke in` inputs and `out` and `choke out` outputs. Dropping an audio clip from the timeline, or a single file from the desktop, on a pad (or on the Sampler) loads that drum, with no need to open the group. Choke is wires: a pad's `choke out` into any number of other pads' `choke in`; the two hats are wired to each other, and there is no graph loop because `choke out` is taken before the gate. The new **Choke Gate** node does the cutting. Event outputs of a group no longer get a meter stage, so notes can leave a group. Any node can now be named (F2 or double-click on its title; the `name` config); the group panel's Controls follow wires through nested groups, so `Kick level` still sets the Sampler's gain. The songs' racks are unchanged, so the golden renders are too. Tests: `choke_gate` (unit), `editor::tests` (pads and renaming), the `instruments` and `synced_effects` tests, route S09 (the hats choke each other). The S09 fixture loads the kit by pad name; dragging onto a pad is not routed yet.


## Sampler Fixed pick and pad `sample` / `take` inputs

The Sampler's `rr_mode` has a fourth choice, **Fixed**, with an `rr_pick` parameter (the slot, from 1, clamped to the slots there are): a set's copies of one drum are chosen by parameter, so the pick can be changed, automated or wired, and no ports change. Each Drum Pad gained a `sample` input (a sample or Sample Set wired over the Sampler's own; a dropped file still fills it when nothing is wired) and a `take` input (into `rr_pick`), and its Sampler is set to Fixed. A single-sample pad is unaffected, so no golden render moved. Prepares drum rack from clip (`docs/design/drum-rack-from-clip.md`, step B). Tests: `sampler` unit test, `realtime.rs`, `editor::tests`.
