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
