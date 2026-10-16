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
What one press of Record captures. It becomes a new clip and section on each armed track, undone as one step.

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

**Clip**:
The thing a section shows: the audio, its clip tempo, beat and warp markers, loop and true length. Copies share one clip until one is modified, and then that one gets its own.

**Warp**:
Re-mapping an audio clip's time so its beats land where the user or the timeline wants them, applied live on playback. A clip's warp can be switched off, which plays it at its natural speed.

**Clip tempo**:
The tempo an unwarped audio clip naturally has: the tempo it was recorded at, or the tempo of the song for an imported file. It can change along the clip, so it is a curve and not one number.

**Beat marker**:
A point in a clip's own time where the clip tempo says a beat falls.

**Grid offset**:
How far a clip's beat lines are slid from the whole beats, in beats, to line them up with the clip's transients. It moves the grid the editor snaps to and warp quantise pulls toward, and the section slips with it (it starts at the same place against the lines), so what plays is the audio under the new lines: the way Ableton's waveform drag shifts a clip against its grid.

**Warp marker**:
A pin a user (or a warp quantise) puts on a clip, tying one moment in the clip, usually a transient, to a timeline beat, so it moves with tempo changes. Warp markers snap to beat markers.

**Tempo conversion**:
The second, automatic warp that carries a clip's beat markers from the clip tempo onto the timeline's tempo at that point. It can be switched off per clip.

**Groove**:
The timing feel of a warp, apart from the warp algorithm: how far a clip's beats sit from the straight grid. A groove, and not the absolute warp, is what one clip can lend another, because the absolute map depends on the algorithm used.

**Warp quantise**:
Pulling a clip's warp markers toward a grid by an amount, without destroying the original positions. Not the same as Quantise (recording), which snaps note starts of a take.

**Section**:
What sits on the timeline: a window onto a clip. A later section on the same track cuts an earlier one where it starts, and the earlier one's tail stays hidden until the later one moves. (In the code a section is still a `Clip`, and what sections share is a `ClipWarp`, named by `WarpId`.)

**True length**:
How long the clip really is after warping. A section shows no more than this unless the clip loops.

**Loop (clip)**:
A clip property that repeats a loop region of the clip, in beats, gaplessly, so a section can be dragged past the true length. The region defaults to the whole clip, and the part before it plays once. Switched on from the right-click menu or the properties panel.

**Clip editor**:
The panel below the arrangement for the selected clip: the piano roll for MIDI, and for audio the clip's own timeline with its waveform and, along the top, its warp markers. Properties stay in the properties panel.

## Stems

**Stem**:
One of the parts an AI model separates a clip's audio into (drums, bass, vocals, other). Stems are made once per clip, ahead of playback: as clips of their own (Split to stems) or in a cache (Stem warp).

**Residual**:
The clip's audio minus its stems, so that stems plus residual add back up to the clip. It holds whatever the model did not assign to a stem.

**Split to stems**:
An action on an audio clip that makes one new clip for each stem, and one for the residual, regardless of whether the clip is warped. The source clip stays, muted. It is one undo step.

**Clip mute**:
A switch on a section that silences it without removing it, so it can be brought back. (New with Split to stems.)

**Stem warp**:
Warping a clip by warping each stem and the residual with the mode that suits it, then adding them back together.

## Warp evaluation

**Warp evaluation**:
Scoring how well Noodle's warp methods change a clip's timing without damaging its sound, by comparing the warped audio with a reference. A dev tool with a small subset that gates CI, not a feature of the app.

**Native render**:
A song rendered straight from its own synths and samples at the target tempo or tempo map, with no warping. It is the answer key for generated songs.
_Avoid_: Ground-truth render, golden render (that one is the regression file in `golden/`)

**Known truth**:
The case where a native render exists, so a warped song is scored against what it should sound like. Only generated audio has it.

**Borrowed truth**:
The case where no native render exists (a recording, a dropped-in mp3), so the score is relative to the output of established external stretchers and to facts the warp map already tells us, such as where each onset should land and that the pitch should not move.

**Excerpt**:
The 15 to 20 s stretch of a generated song, whole bars of its busiest part, that stands in for the song in tests. The whole song is for the dev command.

**Probe**:
A short, controlled signal (a click train, a held chord, a sweep, a lone kick) used to show which kind of sound a warp method fails on. Songs show how a method sounds; probes show why it fails.

**Headline score**:
The warp evaluation's main number for a song: the bare mixed audio with nothing but the file, so the detected clip tempo and the automatic mode choice are part of what is scored. Stems and a fixed mode are reported next to it as diagnostics.

## Presets and the group library

**Preset**:
A named set of one node's parameter values and its config other than what shapes its ports, applied back to a node of the same type that has the ports it sets (found by name, never by position) as one undo step. It never changes wires, automation lanes, mute or solo.

**Group preset**:
A preset for a group: the values and port-neutral config of every node inside it, apart from the group's own boundary controls (gain, mute, solo). It applies only to a group with the same topology; on any other group it is not offered.
_Avoid_: Patch, snapshot

**Topology (of a group)**:
What makes two groups "the same" for presets: the same node types, the same wires between them, the same boundary ports and port names, (a group preset also needs every node to have the parameters it sets, by name). Config such as a sample or an expression does not count, so two kits can be presets of one drum rack. Node IDs, labels and editor positions do not count.

**Group template**:
A saved group in the library. Using one puts an unlinked copy in the project, which stays freely editable and remembers which template it came from.
_Avoid_: Linked instance, prefab

**Library**:
Everything saved for reuse: group templates and presets. System entries ship with the app and are read-only; user entries are the person's own files, shareable by copying them.

**Note split**:
One node that routes MIDI notes to its outputs by note range and velocity range. With one output and no rest output it is a note filter; with several, a splitter. A drum rack is a note split in front of samplers.

**Hidden template**:
A group template that the library does not list and the Add menu does not offer; the app instantiates it itself. A track is one, made only by Add track.
