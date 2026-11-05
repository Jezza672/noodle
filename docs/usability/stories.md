# DAW user stories: advanced user

Thirty goals an experienced user would expect to reach in a serious DAW. Each is a moderately complex, end-to-end task, written from the user's side and not tailored to Noodle's current features. Twenty came from a writer with no knowledge of Noodle; ten were kept from a first draft where they covered ground the other list did not. Difficulty is not assessed yet; the method is in [usability.md](../architecture/usability.md).

Each story has a stable ID so scores, scripts and issues can refer to it. Never renumber: retire a story by marking it retired (S25 and S26 are, so twenty-eight are live), and give a new one the next free ID.

Under each story are its **late changes**: edits to an early decision, made after the story is finished. Each one measures how costly and how destructive it is to change your mind. They were written before anyone checked how Noodle handles them, on purpose.

## Recording

- **S01** **Comp a vocal from multiple takes.** As a producer, I want to record eight loop-cycle takes of a lead vocal into one track, audition them as stacked lanes, swipe the best phrases into a single comp with automatic crossfades at each edit, and still change any section later, so that I get a seamless performance without losing the alternate takes.
  - *Late change:* Re-pick the phrase for verse 2 after effects and automation are on the track.
  - *Late change:* Record a ninth take into the same lanes and use one line from it.
  - *Late change:* Slide the whole comp 10 ms later.
- **S02** **Punch in on a live overdub.** As a recording engineer, I want to set punch-in and punch-out points with a two-bar pre-roll, monitor through low-latency input with my headphone cue mix, and record a guitar fix that drops in cleanly over the existing take, so that the player can repair one phrase without redoing the whole part.
  - *Late change:* Move the punch-out one bar later and re-record.
  - *Late change:* Go back to the take from before the punch.
- **S03** **Drive external hardware like an internal instrument.** As a hardware-synth owner, I want to send a MIDI part and clock to an external synth, record its audio back in, and have the recorded audio line up with the grid after round-trip latency, so that outboard gear behaves like an internal instrument.
  - *Late change:* Change the synth's patch and re-record the part.
  - *Late change:* Edit two notes in bar 5 and re-record only that bar.
  - *Late change:* Change the project tempo after recording.

## Editing and time

- **S04** **Tighten a multitrack drum recording.** As a mixing engineer, I want to detect transients across a group of phase-locked drum mics, quantize them together to a 16th-note grid at 70% strength, and fill the gaps with crossfades, so that the kit sits in time without phase smearing between the close and overhead mics.
  - *Late change:* Raise quantize strength from 70% to 90%.
  - *Late change:* Change the grid to triplet 8ths for the fill only.
  - *Late change:* Restore one fill to its original timing.
- **S05** **Conform audio to a new tempo and key.** As a remix producer, I want to import an acapella at its original tempo and key, detect its tempo map, and time-stretch and pitch-shift it to match my 124 BPM project in F minor with formant preservation, so that the vocal fits my track and sounds natural.
  - *Late change:* Change the project tempo from 124 to 128 BPM.
  - *Late change:* Change the key from F minor to G minor.
  - *Late change:* Swap in an alternate acapella take, keeping the edits.
- **S06** **Separate a song into stems and remix it.** As a remixer, I want to split a finished stereo track into vocals, drums, bass and other, then mute, re-effect and rearrange those parts, so that I can build a remix from a single file.
  - *Late change:* Rerun separation at a higher quality setting, keeping the arrangement edits.
  - *Late change:* Bring back the original full mix for the last chorus only.
- **S07** **Restructure an arrangement by section.** As a songwriter, I want to mark the intro, verse, chorus and bridge with named markers, then copy and paste a selection across all tracks (for example all the clips of a verse, put after an existing chorus), with its automation and tempo coming along, so that I can try a new song structure in minutes.
  - *Late change:* Move the bridge before the second chorus after automation is drawn.
  - *Late change:* Lengthen the chorus by four bars (insert time, then copy).
  - *Late change:* Give the bridge its own tempo.

## MIDI and composition

- **S08** **Edit MIDI performance detail.** As a composer, I want to record a piano part, then use a piano roll and controller lanes to humanise velocities, fix the timing of selected notes only, and redraw sustain-pedal CC64 data, so that the part sounds played rather than programmed.
  - *Late change:* Switch the part to a different instrument; the pedal and velocity edits still apply.
  - *Late change:* Transpose the part down a tone.
  - *Late change:* Halve the humanise amount.
- **S09** **Program a drum pattern with swing and choke.** As a beatmaker, I want to load my own kick, snare and hat samples into a drum sampler, program a two-bar pattern with accents and swing, and have the closed hat choke the open hat, so that the groove feels human.
  - *Late change:* Swap the snare sample.
  - *Late change:* Change the swing amount.
  - *Late change:* Extend to four bars with a variation in bar 4.
- **S10** **Program multi-articulation orchestral parts.** As a film composer, I want to map keyswitches or articulation IDs to named articulations (legato, staccato, pizzicato) per note in the MIDI editor, with the DAW sending the right switches on playback and when playback starts mid-cue, so that sample-library strings play the right articulations without hand-placed keyswitch notes.
  - *Late change:* Switch to a library with a different keyswitch map.
  - *Late change:* Rename an articulation.
  - *Late change:* Start playback mid-cue after the change.
- **S11** **Turn an audio phrase into MIDI.** As a producer, I want to convert a hummed melody or a recorded bassline into MIDI notes, then play them through a synth, so that I can reuse a performance with a different sound.
  - *Late change:* Re-record the hum.
  - *Late change:* Change detection sensitivity after notes have been edited. (Scored as converting again at another setting and replacing the MIDI clip: the conversion is destructive and keeps nothing of its analysis.)
  - *Late change:* Play the MIDI through a different synth.

## Sound design

- **S12** **Build a multisampled instrument from a folder.** As a sound designer, I want to turn a folder of recordings, each named after a note, into a playable instrument using samplers, so that any input note plays the closest recorded note, pitch-shifted by the difference.
  - *Late change:* Replace one recording.
  - *Late change:* Add three more notes to the folder.
  - *Late change:* Change the naming scheme (C#4 to Db4, or MIDI numbers).
  - *Late change:* Change the root tuning or bend range.
- **S13** **Design a modulated synth patch.** As a sound designer, I want to build a layered instrument from two synths and a sampler in an instrument rack, map one macro knob to filter cutoff, reverb send and distortion drive with custom ranges and curves, and save it as a preset, so that a single gesture morphs the whole sound.
  - *Late change:* Add a third layer after the macro is mapped.
  - *Late change:* Change the macro's curve for one target.
  - *Late change:* Update the saved preset and see it in other projects.
- **S14** **Share modulation sources across many parameters.** As a sound designer, I want one LFO, one envelope follower and one random source to modulate several parameters on different devices, each with its own depth, so that a patch moves in a coordinated way.
  - *Late change:* Replace the LFO with a step sequencer, keeping its targets.
  - *Late change:* Change the depth on one target.
  - *Late change:* Add a new target.
- **S15** **Make a tempo-synced rhythmic gate.** As an electronic producer, I want to chop a sustained pad into a sixteenth-note pattern I can edit step by step, synced to the project tempo, so that it pulses with the track.
  - *Late change:* Change the pattern to triplets.
  - *Late change:* Apply the same gate to a second track.
  - *Late change:* Change the project tempo.
- **S16** **Make a vocoder or spectral effect.** As an experimental producer, I want to impose the spectral envelope of a vocal onto a synth chord, or freeze and smear a sound's spectrum, so that I can create effects that are not available as a single plugin.
  - *Late change:* Swap the carrier synth.
  - *Late change:* Change the number of bands.
  - *Late change:* Use a different vocal take as the modulator.
- **S17** **Resample and mangle audio.** As a sound designer, I want to bounce a processed track in place to audio, reverse it, slice it to a sampler by transient, and play the slices from a MIDI controller, so that I can turn existing material into new playable textures.
  - *Late change:* Change an effect on the source track after bouncing.
  - *Late change:* Change slice sensitivity after the pads are mapped.
- **S18** **Use third-party plugins.** As a producer, I want to load my favourite VST3/CLAP/AU reverb and synth, automate their parameters, and save their state with the project, so that I am not limited to built-in devices.
  - *Late change:* Replace the reverb with a different plugin, keeping its automation where it maps.
  - *Late change:* Reopen the project on a machine missing one plugin.

## Mixing

- **S19** **Build a parallel processing mix bus.** As a mixing engineer, I want to route all drum tracks to a drum bus, add a pre-fader send to a heavily compressed parallel bus, and use sidechain input from the kick to duck the bass, with automatic plugin delay compensation keeping everything phase-aligned, so that the low end is punchy and controlled.
  - *Late change:* Add a new drum track to the bus.
  - *Late change:* Change the sidechain source from the kick to kick and snare.
  - *Late change:* Switch the send from pre-fader to post-fader.
- **S20** **Automate a mix in real time.** As a mixing engineer, I want to write fader and plugin-parameter automation in touch and latch modes from a hardware control surface, then trim existing automation by a relative amount over a selected range, so that I can do a performance-style mix pass and refine it afterwards.
  - *Late change:* Move an automation pass to a different parameter.
  - *Late change:* Trim the whole chorus by -2 dB.
  - *Late change:* Use a different control surface.
- **S21** **Recall alternate mix versions.** As a mixing engineer, I want to save mix snapshots or project versions for "main", "vocal up" and "TV mix", compare them A/B while playing, and bounce each one in a single batch export, so that I can deliver every client version without rebuilding the mix.
  - *Late change:* Change the vocal EQ in every version at once.
  - *Late change:* Add a fourth version.
  - *Late change:* Rename a version.
- **S22** **Produce a surround or immersive mix.** As a post-production mixer, I want to set up a 5.1 or 7.1.4 bus structure, pan objects in 3D with automation, fold down to stereo for a check, and export the bed, the object stems and a stereo fold-down, so that the mix can be delivered for streaming and handed to an Atmos renderer.
  - *Late change:* Change the layout from 5.1 to 7.1.4.
  - *Late change:* Move one object's path.
  - *Late change:* Re-export the stereo fold-down.

## Mastering and delivery

- **S23** **Master to loudness targets.** As a mastering engineer, I want to sequence an album of tracks with set gaps and crossfades, apply per-track and master-chain processing, monitor integrated LUFS and true peak, and export 24-bit WAV, 16-bit dithered CD files and a CUE sheet with ISRC codes, so that the release meets streaming and replication specs.
  - *Late change:* The label wants -16 LUFS instead of -14.
  - *Late change:* Swap the order of tracks 3 and 4.
  - *Late change:* Replace track 5 with a new mix.
- **S24** **Deliver stems to a collaborator.** As a producer, I want to export every track or bus as stems from bar 1 to the song's end, at the project sample rate, with or without effects, named consistently and with tails included, so that another engineer can rebuild the mix in their own DAW.
  - *Late change:* Split the guitar stem into two stems.
  - *Late change:* Change the tail length.
  - *Late change:* Re-export one stem after a mix fix.
- **S25** *Retired.* Exchange a session via interchange formats. Dropped by the owner (#362): Noodle is not aiming at post-production interchange.
- **S26** *Retired.* Score and mix to picture. Shelved by the owner (#363): Noodle is not becoming a scoring-to-picture suite.

## Live

- **S27** **Jam live with pads that launch loops and patterns.** As a live performer, I want pads that trigger loops and patterns quantized to the bar, and a tempo that I can tap or sync to external MIDI clock, so that I can improvise a set without stopping the transport.
  - *Late change:* Add a new loop to a pad during the set.
  - *Late change:* Remap a pad.
  - *Late change:* Switch the tempo source from tap to external MIDI clock.
- **S28** **Run backing tracks reliably on stage.** As a touring musical director, I want a setlist of songs, each with its own tempo, click, cues and backing stems on separate hardware outputs, plus MIDI program changes sent to the band's gear, all advancing by footswitch, so that the show runs the same every night with no hands on the computer.
  - *Late change:* Reorder the setlist.
  - *Late change:* Change one song's backing stems.
  - *Late change:* Move to an interface with different outputs.

## Workflow

- **S29** **Organise a large session with templates.** As a working producer, I want to colour-code, group and fold tracks into folders, shrink unused ones to just their name, save the routing and plugin setup as a project template, and use key commands for repetitive edits, so that I can find my way around a 150-track session fast and start every new project ready to work.
  - *Late change:* Start the next project from the template.
  - *Late change:* Regroup the folders.
  - *Late change:* Change the colour scheme.
- **S30** **Keep a large project responsive.** As a producer with a 60-track project, I want to freeze or render heavy tracks and groups, see where CPU is going, and unfreeze to edit later, so that playback stays glitch-free on my laptop.
  - *Late change:* Edit a frozen track and refreeze it.
  - *Late change:* Change the project sample rate.
