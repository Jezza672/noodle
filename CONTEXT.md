# Noodle

A node-based DAW: sounds and mixes are built by wiring nodes together in an editor modelled on Blender's compositor, then arranged in time on a conventional timeline.

## Language

### The project

**Project**:
The single document holding everything: one global graph plus the arrangement, saved as one file.
_Avoid_: Session, song, patch (a patch is a part of a project, not the whole)

**Graph**:
The one global network of nodes and connections that makes up a project. There is never more than one.
_Avoid_: Patch, canvas

**Node**:
A unit of processing with typed input and output ports, a set of parameters and a few config values.
_Avoid_: Module, block, device

**Primitive node**:
A node that implements one base feature (an oscillator, a filter, an envelope, a delay), as opposed to a group.
_Avoid_: Built-in, atom

**Connection**:
A wire from one node's output port to another's input port.
_Avoid_: Cable, link, patch cord

**Frame**:
A labelled box drawn behind nodes to organise a patch. Layout only; it has no effect on sound.
_Avoid_: Comment box, region

### Settings

**Parameter**:
A node setting that can be modulated, so it also appears as an input port.
_Avoid_: Property, knob, control (unless it means a group's exposed control)

**Config**:
A node setting that changes the node's ports or signal shapes, so it cannot be modulated; changing it rebuilds the node.
_Avoid_: Option, property

### Signals

**Audio signal**:
Samples shaped by voices, channels and frames. Audio-rate modulation is just audio; there is no separate CV type.
_Avoid_: CV, control signal

**Events signal**:
A timestamped list of notes, note expressions and MIDI, carried on its own kind of port.
_Avoid_: Note stream, MIDI stream

**Voice**:
One lane of a polyphonic signal. Signals of different voice counts broadcast against each other.
_Avoid_: Channel (a channel is left/right, not a voice), polyphony lane

### Structure

**Group**:
A node that contains a subgraph and exposes ports through it. Buses, sends and synthesisers are all groups.
_Avoid_: Subpatch, container, macro

**Synth**:
A group of primitive nodes whose controls are exposed as group inputs, and which stays editable after creation. There are no synth nodes.
_Avoid_: Instrument node, generator

**Track**:
A group of a particular shape that takes both audio and MIDI clips and carries gain, mute and solo. There is one kind of track.
_Avoid_: Audio track, MIDI track, channel strip

**Track input**:
The node inside a track that plays the track's clips out of its audio and MIDI outputs.
_Avoid_: Clip player, source

**Clip**:
A piece of audio or MIDI placed on a track at a point on the timeline.
_Avoid_: Region, item

**Lane**:
Automation of one parameter over time, stored with the arrangement rather than as a wire.
_Avoid_: Envelope (that is a synthesis node), automation track

**Mixer**:
A view over a Mix node that shows one strip per input. It is not a separate structure.
_Avoid_: Console, mixing desk

### Rendering

**Freeze**:
Rendering a subgraph to audio once so that playback reads the result instead of recomputing it.
_Avoid_: Bounce, commit

**Offline node**:
A node that needs its whole input before it can produce output, so it only ever plays from a cached render.
_Avoid_: Non-realtime node

**Golden render**:
An example project's reference audio, which the current engine must reproduce within a small tolerance.
_Avoid_: Snapshot, baseline
