# M1: Patch and hear

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

The first build that feels like the product.

**Status:** all of M1's code is on main: the app shell (the window, theme,
panels, files, undo/redo and play/stop), the node editor with parameter
widgets on the nodes, the properties panel, the Scope, Meter and Input
nodes (drawn live on their nodes), and the audio settings dialog (File →
Audio Settings…). An end-to-end test follows the "done when" line below
through the app. The review pass is done and its fixes are merged, and CI
passes on Linux and Windows. Left: the listening check on a Mac.

- eframe app shell with Canvas-style dark theme (see `theme.rs`) and panel layout.
- **Node editor:**
  - Pan and zoom.
  - Shift+A to search for and add a node.
  - Drag to connect, Ctrl+right-drag to cut wires.
  - Box select, delete, duplicate.
  - Reroute points and frames.
- Parameter widgets on the nodes themselves and in a properties panel.
- Live editing: structural edits recompile, parameter edits go straight to the parameter cells.
- Compile errors shown on the wires and nodes they come from.
- Audio input node and a device and settings picker.
- Scope and meter nodes, which exercise the path from engine back to UI.
- Save and load, and undo/redo from the UI.

**Done when:** you can build a patch from scratch, tweak it while it plays
with no glitches, save it, and reopen it exactly as it was.

## M1 follow-ups: wire editing

**Status:** done. Delete and Backspace delete the selection (Backspace is
the key a Mac calls Delete, and was the missing one), double-clicking a wire
breaks it, and dropping a node onto a wire splices it in as the design below
describes.

Small node-editor changes requested after M1 landed. They share the editor's
hit-testing and wire code, so they go in one PR, alongside the M2 app work.

- **Delete key on a selected node.** Pressing Delete (and Backspace) with
  nodes selected deletes them, as one undo step. "Delete" is already listed
  under M1's node editor, so first check why the key doesn't work and fix
  that rather than adding a second path.
- **Double-click a wire to break it.** One `Command` removing that
  connection, as Ctrl+right-drag already does. The click's hit-test is the
  same distance-to-curve test the cut gesture uses.
- **Drop a node onto a wire to splice it in.** The wire's source goes to the
  node's input and the node's output goes to the wire's destination.
  *Proposed default design:*
  - **Trigger:** while a node is dragged, the wire nearest the node's body
    (within a small radius, and only if the node has no connections yet)
    highlights. Releasing over it splices. Holding Alt while dragging turns
    splicing off.
  - **Port choice:** the first input and the first output whose signal type
    matches the wire's (audio onto audio, control onto control), preferring
    the main signal port over a parameter port. If nothing matches, the wire
    doesn't highlight and nothing happens. A node with several candidate
    ports takes the first by position, and the user can rewire from there.
  - **One undo step:** remove the old wire, add the two new ones. The
    compiler's usual shape and cycle checks apply, and a splice that would
    create a cycle is refused with the normal diagnostic.

## Backlog: modulation display (M3)

**Done** (M3 batch 1). How it was settled is in "Modulating a parameter" and
"Parameter widgets" in ARCHITECTURE.md. The decision differs from the
proposal below in one way: offsetting in slider space is a per-parameter
setting (`ParamInfo::offset()`), not the rule for every wire, because many
ports take an exact value (pitch, gates, automation lanes, a Mix input), so
the vibrato example and its golden render are unchanged. The text below is
the original proposal.

Both of these are about control signals, so they belong with M3's LFO and
envelope nodes, which are the first sources people will wire into parameters.
The log-versus-linear decision comes first, because the meter's scale depends
on it.

- **Show a modulated parameter's live value.** *Done:* the telemetry probe
  (`Telemetry::read_param`), the properties panel, and the slider on the node
  body as a meter with ticks over the last 3 seconds. A parameter with a wire
  into it currently greys out. Instead, its slider becomes a meter:
  - **Value:** the fill shows the parameter's current effective value, read
    every frame.
  - **Range marks:** two ticks mark the minimum and maximum over the last few
    seconds (default about 3), so the modulation depth is visible against
    the parameter's range.
  - **Plumbing:** a connected parameter port gets a telemetry tap, which
    keeps a per-block minimum, maximum and last value in atomics, in the same
    way as `Meter`'s peak. The tap is only added to ports that are wired and
    visible, and is off the audio path otherwise. Real-time rules apply: no
    allocation, and `realtime.rs` is extended to cover it.
  - **Scaling:** the meter uses the parameter's own taper, so it lines up
    with the slider it replaces.
  - **Oscillator levels:** a source's output is shown relative to the
    target's range ("this LFO sweeps 30% of cutoff"), not in dB.
- **Log versus linear for control signals.** *Proposed default design:*
  - **Signals stay linear.** Audio and control signals are plain floats, and
    gain is linear amplitude. Only the UI shows dB, as in the Meter node.
  - **Parameters carry a taper** (linear or log, already in `ParamInfo` for
    frequency) that decides how a value maps to the slider. Modulation adds
    in the parameter's *tapered* space, so a bipolar LFO of depth 0.5 moves
    a log cutoff by the same number of octaves at any base frequency, and a
    linear parameter moves by the same amount everywhere.
  - **Where it's applied:** the engine converts at the parameter, once per
    block: `value = from_taper(to_taper(base) + depth * signal)`, clamped to
    the range. A wire into a parameter port therefore means "offset in the
    slider's space".
  - **Exceptions:** a dedicated exponential converter node (for V/oct pitch
    and dB gain) is available for cases where the user wants the other
    behaviour.
  - **To confirm:** this changes what a wire into a log parameter does today
    (it adds in raw units), so it needs a golden-render check, and
    `examples/vibrato.ron` is the first one to look at.
