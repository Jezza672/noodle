# M9: Library, presets and more nodes

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** batch C (#156) restored the 30 Hz low cut and whole-mix compressor on
funk-engine and breakbeat-run; the tempo detector copes with them since the
M8 closing onset fixes, and a regression test covers it. Batches 2 and 3 have
landed: the nodes (Sampler, Note Split, Dynamics, Parametric EQ, Reverb, Arpeggiator, with graphs for Dynamics and EQ
and a "Load sample…" picker), and the warp evaluation songs, whose drums are a
drum rack of committed samples (`noodle-eval/songs/samples`) and whose mixes
run through Dynamics nodes set by the compressor and limiter node presets, a
Parametric EQ and a Reverb. Batch 1 (library and
presets) is done in code: it left out samples by content hash in the library
(the Sampler's store now exists) and shipped no node presets (batch 5 added
them); the system presets are group presets of the instrument templates. The
design is in `CONTEXT.md` (Presets and the group library) and ADRs 0004 and
0005.

**Batch 6:** "Update from template" on a group made from a library template
(right-click; replaces the inside, keeps wires at its ports and its own level,
one undo step); see ARCHITECTURE.md. The same batch fixed #167: the eval
harness warped Auto as if the project were already at the target tempo, so a
clip with no detectable tempo (lone kick, sweep) was never stretched. Auto is
now warped as a user does it (imported at the song's tempo, tempo changed
afterwards), and it scores like Grain on those probes.

**Batch 4 (more nodes and free automation tracks):** ten nodes (Envelope, a
drawn curve played on a trigger with a multi-voice or single-voice setting;
Distortion; Bitcrusher; Chorus; Phaser; Envelope Follower; Sample & Hold; Lag;
Comparator; Clock) and the Automation node, which turns a timeline lane into a
0 to 1 signal wired to any input. The timeline's "+ Auto" button adds one and
draws its lane in a row of its own. Batch 5 added the
Envelope's length in beats (`time` Seconds or Beats, and `beats`), a curve
preview on its body, and system presets for every node (`nodes.ron`, Dynamics
included: Compressor, Limiter, Gate, Expander and variants).

1. **Library and presets.** Node presets (parameters and port-neutral config, same node
   type and port layout, one undo step, never touching wires, lanes, mute or
   solo). Group
   presets, which apply only to a group with the same topology (node types, wires, boundary ports
   and port layouts; IDs, labels, positions and config that does not shape
   ports ignored).
   Group templates: unlinked copies, system ones embedded and read-only, user
   ones one `.ron` file each in the user data dir. A track is a hidden
   template, instantiated only by Add track. UI: a Presets control in the
   properties panel, a searchable Groups section in the Add menu, and "Save
   group to library". Samples are stored by content hash. The existing
   instrument examples ship as the first system templates.
2. **New nodes.** `Note Split` (one output is a note filter; per-output note
   and velocity ranges, `transpose`, optional `rest` output), `Sampler` (one
   sample, root key, loop, one-shot or gate), `Dynamics` (compressor, limiter,
   gate and expander as a `mode`; sidechain; gain-reduction output; lookahead
   reported to plugin delay compensation; a transfer-curve graph with
   draggable points and an attack and release plot), Parametric EQ, Reverb and
   Arpeggiator. Drum rack and multi-zone instruments are groups, and Compressor, Limiter and
   Gate are presets of Dynamics, never monolithic nodes. Each node ships with
   presets.
3. **Warp evaluation songs.** Once the new nodes exist, the generated songs in
   `noodle-eval/src/song/` use them: the drums become a drum rack (a Note
   Split in front of Samplers, closing the sampler gap filed as #143), and the
   instruments and mixes gain Dynamics, EQ and Reverb. The songs' `.ron`
   projects and `baseline/songs.json` are regenerated, and the native renders
   and scores are checked so the warp evaluation sees sampled drums, compressed
   mixes and reverb tails, which the synthesised drums and dry mixes do not
   test.
