# MIDI input, MIDI clips and recording

A MIDI port is chosen in the audio settings dialog (`AudioConfig::midi_input`,
a port name, saved with the other device preferences). The choice belongs to
the app, not the project, like the audio device, and changing it doesn't
restart the audio.

- **From the port to the graph.** `noodle-io/src/midi.rs` opens the port with
  `midir`. The driver calls back on its own thread, which keeps only whole
  channel messages (system exclusive and real-time bytes are dropped) and
  pushes each as three bytes into a ring buffer per `MidiBus` subscriber.
  Each `MIDI In` node subscribes when it is built and pops its own buffer on
  the audio thread, so neither side locks, waits or allocates; a node that
  falls 1024 messages behind misses the newest. Dropping the node
  unsubscribes it.
- **What `MIDI In` outputs.** Note-ons and note-offs become note events (a
  note-on with velocity 0 is a note-off, velocity is 0 to 1), with a note ID
  made from the channel and key and its top bit set, which keeps it apart
  from the IDs of notes in clips. Controllers, pitch bend, pressure and
  program changes pass on as raw `Midi` events. All notes off and all sound
  off end the notes it still holds. Its `channel` parameter (0 for all)
  filters.
- **Timing.** Every message is stamped with its arrival time on a monotonic
  clock (`noodle_io::now_micros`), under the bus's lock so stamps are in
  queue order. Each device callback tells the bus's `AudioClock` when it ran,
  and for each engine block inside it which span of arrivals that block
  plays: the callback's length before the block's own place in it. A note
  played at some point of one device period therefore sounds at the same
  point of the next, in whichever block that falls, and what is later than
  the block's span waits for the next. Live playing is one device period late
  with no jitter beyond the callbacks' own, however long the period is. The
  callback also publishes an anchor, a pair of the clock and the transport's
  sample, that recording reads (see "MIDI takes"). Without a device the node
  plays whatever arrived since its previous block began. The app and
  `noodle play` use 128-frame engine blocks (about 2.7 ms), and renders do
  too, so a render matches what was heard; that costs more per-block overhead
  than 512 did. Events a node emits are kept in time order even if the bus
  hands over two messages out of order.
- **The computer keyboard.** A toolbar button (or M) turns the home row into
  an octave of keys, as in Ableton: A W S E D F T G Y H U J K and O L P ;
  are the notes from C3 (60), Z and X change the octave, C and V the
  velocity. The keys are sent to the same bus as a hardware port and are
  taken out of the frame's input, so nothing else (X to delete, say) acts on
  them; a text field, a dialog or the window losing focus lets go of held
  notes. It is app state, off at start, and not saved.
- **Auditioning.** The piano roll's keyboard holds keys down on the clip's
  own track input (`ClipFeeds::audition`, a bitmask in the node's shared
  state, so no queue and no allocation), and the track input turns changes
  into note events on its `midi` output, with note IDs in their own range.
  So the track's synth plays them, other tracks and `MIDI In` nodes don't,
  and they can't collide with a live keyboard. They sound whether or not the
  transport runs. A key still down when the roll closes is let go.
- **Channel filter.** A held note's note-off always gets through, so changing
  `channel` (which can be modulated) can't strand a note; all notes off ends
  the rest across blocks if the event buffer fills.

- **MIDI 2.0.** The bus carries Universal MIDI Packets (`noodle_io::ump`):
  type 2 (a MIDI 1.0 message) and type 4 (MIDI 2.0 channel voice, 64 bits);
  `MidiBus::send` makes a type 2 packet, `send_packet` takes any, and
  `PacketReader` splits a word stream. Receivers see packets
  (`pop_packet`), or MIDI 1.0 messages narrowed from them (`pop_timed`, for
  the take recorder). `Midi2` decodes and encodes type 4 and converts to
  and from MIDI 1.0 with the specification's bit scaling (`widen`). `MIDI In`
  plays MIDI 2.0 as follows: velocity is 16 bits (a note-on of 0 is the
  softest note, not a note-off); a pitch attribute becomes a `Pitch`
  expression (the fraction of a semitone from the key); poly pressure becomes
  `Pressure`; per-note pitch bend becomes `Pitch`, scaled by the node's
  `note_bend` parameter (semitones at the extreme, 48 by default as in MPE);
  per-note registered controllers 1 and 74 become `Timbre`, 7 `Volume`, 10
  `Pan` and 3 (pitch 7.25) `Pitch`; assignable ones and note management are
  ignored. What is channel-wide: controllers, pitch bend and channel pressure go
  on as `EventKind::Channel { channel, control, value }` with the full 32-bit
  value (`EventKind::midi1` narrows one to the MIDI 1.0 message for plugin
  hosts, AU and the sandbox, which speak 7 and 14 bits); program changes with
  their bank, registered and assignable parameters are narrowed to the raw MIDI 1.0
  events the rest of the engine speaks. Groups are merged and the `channel`
  parameter filters channels as before. Not done: devices (midir delivers MIDI 1.0
  bytes, so hardware is upgraded by the system if at all; packets come from
  `send_packet`, tests and later sources), sysex and flex data, MIDI-CI
  (it needs a MIDI 2.0 device to talk to), and MIDI clips with 32-bit values
  (a note's velocity is already an `f32`).

## MIDI clips

A MIDI clip (`ClipContent::Midi`) is a length and a list of notes, all in
ticks: a note has a start from the clip's start, a length, a key (0 to 127)
and a velocity (0 to 1). Unlike an audio clip's, its length follows the tempo,
because it is music and not a recording. A clip's notes can be in any order;
the commands that add and change clips check them (no empty notes, none
before the start, keys and velocities in range).

- **Playing them.** `ClipFeeds::update` turns each note into samples with the
  tempo table (the note's end is cut at the clip's end, and a note that starts
  past the end is dropped) and sends the sorted list to the track input along
  with its audio schedule, so one version number covers both and an offline
  render waits for both. The track input's `midi` output gets a note-on at the
  sample a note starts and a note-off where it ends. Note IDs in clips use the low 29 bits, so they can't collide with live MIDI (top bit), the
  piano roll's keyboard or live monitoring. A note is known by its
  clip, start and key, so editing other notes while it sounds doesn't cut it;
  if the note itself is moved, resized or removed, the old one ends at once.
  At most 128 notes sound at once on a track.
- **Looping.** A clip has `loop_on`, `loop_start` and `loop_len` (ticks from the
  clip's start; at least a sixteenth of a beat). Looped, it plays from its
  start to the loop's end, then the loop section again until the clip's end.
  Notes before the loop's start are a lead-in that plays once, notes starting
  after the loop's end never play, and a note is cut at the loop's end on
  every pass. `MidiClip::played` works the notes out (the first pass plus
  `repeats`) and both the scheduler and the arrangement preview use it, so
  what is drawn is what sounds. Trimming a looping clip's end keeps notes
  past it (the loop can grow back); trimming its start moves the loop with
  its notes (and ends it if the cut passes it); splitting a looping clip
  gives two that don't loop, holding the notes as they sound.
- **Stopping and jumping.** When the transport stops, or a block doesn't
  continue from the one before (a seek or the loop's wrap), every sounding
  note gets a note-off at the start of that block, so nothing hangs. Playing
  from the middle of a note doesn't start it.
- **In the arrangement.** A MIDI clip is drawn as a block with its notes
  shown small. It moves and trims like an audio clip, with two differences:
  it has no fades, and trimming the left edge keeps the notes where they
  sound (the ones before the new start are cut off or shortened) instead of
  shifting them with the edge. The lane's right-click menu has *New MIDI
  clip*, a bar long at the beat nearest the click.
- **Loop controls.** The piano roll's toolbar has a Loop checkbox (the whole
  clip at first) and, while looping, the loop's start and length in beats.
  The ruler shows the loop as a brace: drag its ends to resize and its body to
  slide it, on the snap grid. Later passes are drawn faintly in the grid and
  the time after the loop's end is dimmed. Audio clips' loops are the warp's
  loop region (M7), not these fields.
- **The piano roll** opens below the arrangement when a MIDI clip is
  double-clicked. Dragging on empty space draws a note, dragging a note moves
  it (in time and pitch, a whole selection together), dragging its right end
  resizes it, Delete removes, the arrow keys transpose and nudge, and a bar in
  the velocity lane sets velocity. Notes snap to the app-wide snap step (the
  *Snap* menu in the menu bar: a beat down to 1/16 of a beat, or off; one
  setting for the arrangement, the piano roll and the clip editor, `snap.rs`),
  Alt turns snapping off. In the piano roll, snapping is on the absolute grid
  the roll draws (`grid::snap_to`, which follows the time signature), whatever
  tick the clip starts on: the roll converts a clip-relative tick to an
  absolute one, snaps it and converts back (`notes::snap`). Each gesture is one undo step,
  worked out from the notes as they were when it began. Notes carry an `id`
  that is unique in their clip (the project assigns one to any note that
  repeats an earlier id when a clip enters it, which also fixes files from
  before ids), and the selection is a set of ids, so it follows its notes
  through undo and redo.

## Recording into the arrangement

A track is armed in the session, which is app state and not saved with the
project. It has two arm buttons, RA for audio and RM for MIDI, each
independent: audio clips go on the audio-armed tracks, MIDI clips on the
MIDI-armed ones, and only RM also monitors live (audio has no live monitoring
yet). A take with no audio-armed track starts no file. Record needs an armed track and a saved project,
since takes are stored next to it, in a `<project name> recordings` folder as
`take-001.wav`, `take-002.wav` and so on, and a clip's source is a path
relative to the project file like any other. Record starts the engine and the
transport if they aren't running and starts `Playback::start_recording` at
the playhead. Ending the take (the button, stopping, pausing or seeking the transport, or
the engine being turned off or closing, say for an output change) stops the recorder and adds one
audio clip of the whole take, starting where recording began, to every armed
track as a single `Batch`, so one undo removes the take. An armed track that
was deleted meanwhile gets no clip (if all were deleted, the file is kept and
said so), and a take with no audio adds nothing and
deletes its file. Input that was dropped because the disk stalled is
reported. What the player played arrived after a round trip: the output
latency they heard the project through (the driver's `playback - callback`
time plus plugin delay compensation) and the input's own
(`callback - capture`, kept by the input callback in `Recorder::input_latency`).
The clip starts at the playhead but skips that many frames of the file
(`recorded_clip`), so the take lines up with the project as heard. MIDI takes
subtract the output side only.

**The take in progress** is drawn on the arrangement as it grows: a red clip
from the take's start to the playhead on each armed track, with the notes
played so far (`MidiTake::clip_so_far`) and, for audio, a waveform from peaks
the writer thread keeps while recording (`Recorder::live_peaks`, a few ms
behind). `Session::take_preview` and `take_waveform` feed `timeline/take.rs`.

**MIDI takes.** The same Record also captures MIDI. It subscribes to the
session's MIDI bus for notes (hardware ports and the computer keyboard alike)
and keeps each message with its arrival time. Every UI frame the take notes
the audio thread's newest anchor with what it collected, and maps each
message to the timeline through it, so the stream's startup delay and clock
drift don't matter. Times are moved earlier by the output latency the device
reported (`Playback::output_latency`), since the player was playing along to
what they heard, and the clip starts that much before the playhead was when
Record was pressed. Like an audio take, a MIDI clip goes on every MIDI-armed track,
whether or not it takes MIDI. The
take ends with one MIDI clip per armed track from the start of the take: a note still held ends where the take
does, a note-off with no note-on is ignored, and a take with no notes adds
no clip. The audio and MIDI clips of a take are one `Batch`. Audio is recorded
only when an input is chosen in the audio settings; without one a take is
MIDI alone. The Q menu in the transport sets a grid (off, 1/4, 1/8, 1/16,
1/32, 1/8 and 1/16 triplets) that the starts of the notes snap to when the
clip is made, keeping their lengths, shortening a note that now runs into the
same key's next one and dropping a second note on the same tick. What is
heard while playing is never snapped. The raw timing is not kept.
