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
6. **One audio pane for clips and samples (#231).** The sample view and the
   unwarped clip editor share `clip_editor/pane.rs`: seconds ruler, zoom and pan
   (also middle-drag), the sample curve when zoomed in, the lit part with draggable
   start and end, loop ends, and slip. A clip's edges trim it; a Sampler's set
   `start`/`end`; dragging the audio slips the clip's offset or moves the
   Sampler's region.
7. **Clip and sample parity.** "Anything you can do to a clip you can do to a
   sample", and the other way round. A Sampler's sample now opens in the clip
   editor itself (`clip_editor/subject.rs`: one `Subject`, a clip or a Sampler,
   with the same commands either way), and gained what a clip had:
   - a **gain envelope** (`gain_env`, drawn and edited on the waveform);
   - a **playhead** for each sounding voice (Sampler voice positions reported
     through `Telemetry::open_positions`);
   - a **warp** (`warp`: tempo, follow tempo, markers, grid offset, quantise,
     Detect and Tap, edited on beats in the same view, with a Warp section in
     the Sampler's properties). The Sampler plays it as Repitch;
   - and the other direction, a **loop on an unwarped clip** (a natural-speed
     warp that holds the loop; the clip stays in the seconds view, with the
     toolbar's Loop checkbox and pullable loop ends).
   Not carried over then: Grain and Beats warp modes for samples, the gain
   envelope in the beat view, and a sample loop shorter than a beat in the
   beat view. Grain and Beats followed (see "Shared warp engine" below).

## Shared warp engine for samples

A Sampler now has the clips' warp modes. Grain and Beats run the clip engine's
`Stretcher` over the whole sample on a worker (`sampler_stretch.rs`), and the
voices play the stretched copy with the existing pitch, loop and envelope logic,
mapped back to the sample through the clip's `WarpMap`. The mode picker is shown
for samples. While the copy is made, and while a marker is dragged, the warp
plays as Repitch; an offline render waits for the copy. Also in this change:

- `gain_env` and `warp` are *live config* (`NodeType::live_config`/`retune`), so
  dragging an envelope point or a marker no longer rebuilds the Sampler and cuts
  its notes.
- `Telemetry::read_positions` lists the voices of every instance still
  reporting (a Sampler inside a voice-instanced group, or two instances across a
  plan swap) instead of the newest instance's only.
- `DSP_GENERATION` is 6, since a Grain or Beats warp on a Sampler used to render
  as Repitch.

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
