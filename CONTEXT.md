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

**Warp**:
Re-mapping an audio clip's time so its beats land where the user or the timeline wants them, applied live on playback. A clip's warp can be switched off, which plays it at its natural speed.

**Clip tempo**:
The tempo an unwarped audio clip naturally has: the tempo it was recorded at, or the tempo of the song for an imported file. It can change along the clip, so it is a curve and not one number.

**Beat marker**:
A point in a clip's own time where the clip tempo says a beat falls.

**Warp marker**:
A pin a user (or a warp quantise) puts on a clip, tying one moment in the clip, usually a transient, to a beat. Warp markers snap to beat markers.

**Tempo conversion**:
The second, automatic warp that carries a clip's beat markers from the clip tempo onto the timeline's tempo at that point. It can be switched off per clip.

**Groove**:
The timing feel of a warp, apart from the warp algorithm: how far a clip's beats sit from the straight grid. A groove, and not the absolute warp, is what one clip can lend another, because the absolute map depends on the algorithm used.

**Warp quantise**:
Pulling a clip's warp markers toward a grid by an amount, without destroying the original positions. Not the same as Quantise (recording), which snaps note starts of a take.

**Clip (underlying)**:
The thing a section shows: the audio, its clip tempo, beat and warp markers, loop and true length. Copies share one clip until one is modified, and then that one gets its own.

**Section**:
What sits on the timeline: a window onto a clip. A later section on the same track cuts an earlier one where it starts, and the earlier one's tail stays hidden until the later one moves. (In the code a section is still a `Clip`, and what sections share is a `ClipWarp`, named by `WarpId`.)

**True length**:
How long the clip really is after warping. A section shows no more than this unless the clip loops.

**Loop (clip)**:
A clip property that repeats a loop region of the clip, in beats, gaplessly, so a section can be dragged past the true length. Switched on from the right-click menu or the properties panel.

**Clip editor**:
The panel below the arrangement for the selected clip: the piano roll for MIDI, and for audio the clip's own timeline with its waveform and, along the top, its warp markers. Properties stay in the properties panel.
