# MIDI files (#540)

Standard MIDI File import and export. Deliberately small: MIDI 1.0 notes and controllers, no session interchange.

**Reader and writer.** `noodle-io/src/smf.rs`, on `midly`. `read_smf` rescales every tick to 960 per quarter (rounding), splits a format 0 file by channel, folds `NoteOn` with velocity 0 into a note off, and refuses SMPTE time division and format 2. `write_smf` writes format 1 at 960: a conductor track (tempos, signatures), then one file track per exported track. Note offs sort before note ons at one tick.

**Mapping.** `import` turns a file track into a `MidiClip` (notes matched per channel and key, a stuck note ends at the track's end, CCs, bend, channel pressure and program changes as `ControlPoint`s, poly pressure as a `Pressure` note expression on the sounding note), rounded up to a whole bar of the file's own meter. `export` unrolls looping clips with `played` and `played_controls`, bakes in humanise, and samples ramps every sixteenth of a beat. Pitch bend maps 8192 to 0 asymmetrically, so a full bend round-trips. Not carried: sysex and text events other than the track name, note expressions other than pressure, articulations, take folders (the comp is written). The notice lists what was dropped.

**In the app.** Dropping a .mid on the timeline, or File > Import MIDI…, makes a track per file track (named from the track-name event), with the first on the track it landed on; tracks, clips and (when the project has no clips) the file's tempo map are one `Command::Batch`, so one undo. A project that already has clips keeps its own tempo and meter, and the notice says so. Imported tracks have no instrument; the notice says that too. File > Export MIDI… writes the selected clip, else the selected track's MIDI clips, else the arrangement. `noodle midi-export project.ron out.mid` writes the arrangement.

**Tests.** The smf unit tests (rescale, format 0 split, stuck note, tempo map, meter snap, poly pressure), the round trip, and the app test that drops a two-track file and undoes it in one step. Proven to fail: scaling by 480 instead of 960 fails five of them, and writing a constant pitch bend fails the round trip.
