# Scripting node (done)

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

`Script` (`noodle.util.script`): a per-sample program whose text declares the
node's ports, with state, `if`, the Math functions and `rand`. See
the docs/architecture/ files (index: ../ARCHITECTURE.md) ("There are no synth nodes", the `Script` paragraph). Math
stays as the one-line form.

# MIDI 2.0 (done)

Universal MIDI Packets on the MIDI bus (`noodle_io::ump`) and MIDI 2.0 channel
voice messages played by `MIDI In`: 16-bit velocity, note pitch attributes,
per-note pressure, pitch bend and controllers as note expressions, the rest
narrowed to MIDI 1.0 events. See the docs/architecture/ files (index: ../ARCHITECTURE.md), "MIDI input". Delay lines
for spectral wires (`SpectralDelay`) came in the same batch.

## Follow-ups delivered later

- ~~AU hosting.~~ **Done** (macOS): effects, instruments and music effects, with ports, notes, state and the transport; editors are the follow-up. See "AU hosting" in the docs/architecture/ files (index: ../ARCHITECTURE.md).
- ~~Script node follow-ups.~~ **Done**: bounded `for` loops, arrays (delay lines) and event ports (note in, note out).
- ~~MIDI 2.0 follow-ups.~~ 32-bit controller, bend and pressure events and a `MIDI Control` node are **done**. Still open and blocked on hardware or the MIDI library: device input as UMP (midir delivers MIDI 1.0 bytes) and MIDI-CI.
- ~~Downbeat model.~~ **Done**: the beat and the off-beat are told apart by where the harmony changes (chroma novelty), so `funk-engine`'s phase is right.
- ~~Surround and multichannel buses.~~ **Done**: channel layouts (mono to 7.1), Surround Pan, Convert Layout, Split and Merge Channels, a device channel count, multichannel export. See "Surround and multichannel buses" in the docs/architecture/ files (index: ../ARCHITECTURE.md). The panner display (a draggable source among the speakers) and per-speaker routing on the Output node came after.

## Node monitoring (#208)

A monitor button in each node's header listens to its first audio output through a selectable monitor device, tapped after mute/solo; shift sums more nodes, Ctrl/Cmd silences the other outputs on the device, Ctrl+Shift silences all, Escape stops. See [transport-and-devices.md](../architecture/transport-and-devices.md#node-monitoring).
