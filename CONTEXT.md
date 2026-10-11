# Noodle

A node-based DAW: a node editor for audio, with a timeline.

## Language

**Computer keyboard input**:
A MIDI source that plays notes from the typing keyboard, switched on by a toolbar button. It is a live source like a MIDI port and is not saved in the project.

**MIDI source**:
Anything that produces live MIDI: a hardware port or the computer keyboard.

**Armed track**:
A track that Record writes a take to.

**Take**:
What one press of Record captures. It becomes a new clip on each armed track, undone as one step.

**Quantise (recording)**:
Snapping the starts of a take's notes to a grid when the take is committed. Monitoring while recording is never quantised.

**Latency compensation**:
Shifting a take earlier by the output device's latency, so it lines up with what the player heard.
