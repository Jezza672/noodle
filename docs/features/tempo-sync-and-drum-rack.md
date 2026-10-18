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
