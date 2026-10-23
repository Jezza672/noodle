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

## Random Select (#211)

A primitive node, `noodle.util.randselect`: one output routed from one of N inputs,
chosen at random on each trigger. Signals or notes (config `kind`), uniform pick
with an optional "don't repeat" and a seed, instant or zero-crossing switching
for signals, and finish-or-cut for held notes. Design in
[synths-and-library.md](../architecture/synths-and-library.md). Tests:
the node's unit tests (picks, seed determinism, avoid repeat, zero crossing,
held notes) and `realtime.rs`.

Not done, in [ROADMAP.md](../ROADMAP.md): the editor does not yet set `kind`
from the first wire (the "one universal port type until connected" behaviour)
or grow the inputs as you wire; both are config set by hand for now.
