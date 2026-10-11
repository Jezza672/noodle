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

**Engine**:
The audio stream that runs the graph. It starts with the app, is on by default, and a toolbar toggle turns it off. It runs whether or not the Transport does.

**Transport**:
The moving timeline. It plays, pauses and rewinds independently of the Engine.

**Live monitoring**:
An armed track hearing live MIDI through its own synth, with no `MIDI In` node wired and without the Transport running.

**Virtual output**:
An output device that plays to nowhere. It is the last fallback when no real output device can be opened, and can be chosen like any device. The Engine runs on it as normal, in real time, and discards the audio.

**User action (in tests)**:
One gesture a person makes, sent to the app as the pointer and keyboard events it would see: a drag, click, double click, key press, wheel turn or file drop. All tests perform them through one shared set of helpers.
