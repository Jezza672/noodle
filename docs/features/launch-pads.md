# Launch Quantise and Loop Pads

Delivered (#364). Open work is in [ROADMAP.md](../ROADMAP.md), the design in [../architecture/synths-and-library.md](../architecture/synths-and-library.md).

Story S27 is reworded to "jam live with pads that launch loops and patterns, quantised to the bar, with a tapped or synced tempo". There is deliberately **no clip grid or session view**: Noodle is not copying Ableton's session view. Pads are built from nodes instead.

- **Launch Quantise** (`noodle.event.launch_quantise`): a note-wire node that holds notes until the next beat, half bar, bar, 2 or 4 bars. *Hold* mode quantises note-offs as well (a tap still plays one grid unit); *Toggle* mode makes each key a switch, with a second press before the start cancelling the launch. Raw MIDI passes through. Fixed-size queue (128), so nothing allocates on the audio thread. Tests in `launch_quantise.rs`, and `launch_quantise_never_allocates` in `noodle-nodes/tests/realtime.rs`.
- **Loop Pads** library template (`system:template:loop-pads`): Launch Quantise (Bar, Toggle) into a Note Split and eight Samplers in Gate mode with loop on, on keys 36 to 43, each with a level input. Load a loop into a Sampler by dragging an audio clip onto it. Every group input is exposed (`grid`, `mode`, the levels), so the group is editable after creation.
- **S27 route** now builds the pad, starts the transport, presses the pad and listens for the loop; status Partial (no Ableton Link, #365; loading a loop and the late changes are not routed).
- **S25 and S26 retired** (#362 and #363 closed): marked retired in `docs/usability/stories.md`, IDs not reused, and removed from the registry and the routes. The scorecard now counts 28 stories.

Not done: a controller-note pad template with LEDs or feedback, and following the tempo of a synced clock to the beat phase (see [tempo-source-and-monitoring.md](tempo-source-and-monitoring.md)).
