# Mix versions and articulation maps

Delivered. Closes #358 (S21) and #351 (S10). The design choices are in `/triage/story-gap-solutions/` (batch 2, options B and A).

## Mix versions (#358)

1. **The version list is project state.** `Project::versions` (names, in order) and `active_version`, saved in the project file and set by `Command::SetVersions { names, active }` (`noodle-core/src/versions.rs`). Helpers build the commands for add, remove, move, select and rename, so each is one undo step. At most 16 versions; the last one cannot be removed.
2. **Version Value node** (`noodle.mod.version_value`, `noodle-nodes/src/version_value.rs`). Config `versions` is the number of slots, and the project keeps it equal to the list's length: adding a version also adds a slot, holding the value the active version has, to every Version Value node, in the same command (removing and moving shift the slots too). Each slot (`v1`...) is a position from 0 to 1 along `min` to `max` (`taper` linear or log), so any parameter's slider is easy to drag. The output glides to the active slot over about 15 ms.
3. **Switching needs no rebuild.** The active version travels in `Transport::version` (an atomic on `TransportControl`, set by `Controller::update_project*` and by the session when `SetVersions` is applied). Renders downstream of a Version Value are cached per version (`node_key` in `noodle-engine/src/cache.rs` hashes the active version).
4. **Vary per version** is a menu entry on every parameter (node body and properties panel), `Edit::VaryPerVersion` (`session/versions.rs`): it adds a Version Value node with every slot at the parameter's current value, wired to the parameter, and starts the list ("Main", "Alt") if the project has none. A parameter a wire offsets (a cutoff) gets the range -1 to 1 along its travel with 0.5 as no move, as Automate does.
5. **Version menu** in the transport bar (shown once there are versions): switch, rename the active one, add, remove.
6. **Batch export.** The export dialog lists the versions to write. Each ticked version renders as its own pass on a copy of the project with that version active (`Project::with_active_version`), and files are named `<song> - <version>` (and `<song> - <version> - <track>`) in the chosen folder. `spawn_export_all` runs the passes as one job, with one progress bar (`Progress::set_outer`).

Not done: routing cannot vary, only parameters (a TV mix without vocals is a mute). The S21 route stops before "Export…", since choosing the folder is a native dialog.

## Articulations (#351)

1. **The list** is `Project::articulations` (ID, name, colour; `Command::SetArticulations`). Notes hold the ID (`MidiNote::articulation`), so renaming is one edit of the list.
2. **The track sends the change.** `ScheduledNote::articulation` reaches the track input, which sends `ChannelControl::Articulation` (a channel event whose value is the ID, 0 for the default) at the note's frame, ahead of the note-on, whenever it differs from the last it sent. After a jump or at the start of playback (`chase_articulation`) it sends the articulation of the newest note before the playhead, so a start mid-cue gets the right switch. A track whose notes have none sends nothing. The event has no MIDI 1.0 form.
3. **Articulation Map node** (`noodle.event.articulation_map`, `noodle-nodes/src/articulation_map.rs`) between the track and the instrument. Its table (config, one line per ID) asks for each articulation with a keyswitch (key, velocity, length, held for that long), a controller or a program change. Keyswitch notes use IDs of their own and are let go after a jump. The Properties panel edits the table.
4. **Piano roll.** The Articulation menu in the toolbar gives the selected notes an articulation, adds to the list and renames; notes are tinted by their articulation.

Not done: MIDI 2.0 note attributes as a way to ask, a lane under the roll (the menu does the painting), and per-library map presets in the library (a map is saved and loaded like any node).

## Tests

`noodle-core` (`versions.rs`: the commands and the save round trip), `noodle-nodes` (`version_value.rs`, `articulation_map.rs`, `tests/track_input_midi.rs` for the order and the chase, `tests/realtime.rs` for no allocation), `noodle-app` (`session/versions.rs`, the effect of the commands, routes S10 and S21).
