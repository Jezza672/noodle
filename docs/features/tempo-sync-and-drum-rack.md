# Tempo sync and the drum rack

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** done (ADRs 0008 and 0009). Closes #142 and #143, which turned out to be half
built already: the Sampler and Note Split came with M9 batch B, and the songs'
drums are a rack of them.

1. **Tempo node.** `noodle.mod.tempo`: a `division` choice, and `time` (seconds)
   and `rate` (hertz) outputs that follow the transport's tempo. Allocation
   test, stateless, tests at two tempos.
2. **Library templates.** `Tempo Echo` (Tempo, Lag, Delay with feedback, wet mix),
   `Tempo Tremolo` (Tempo into an LFO into a VCA) and an empty-slot `Drum Rack`,
   each with `division` or per-slot controls as group inputs, generated with the
   other system templates.
3. **Not in scope.** The warp evaluation songs still leave echo and LFOs out, so
   their baselines do not change; putting a synced echo in them is a decision for
   the evaluation, not for this batch.

## Node editor polish (#204, #206, #207, #225)

- **Group output meters.** A group's output ports always keep their
  `noodle.group.stage` in the compiled graph (and so in a freeze's cache), and
  the stage reports its output level like Gain does. The group node and its
  output node draw one bar per channel of every output port.
- **Hover highlight.** The node under the pointer gets a lighter outline; the
  other nodes' fields no longer draw in the disabled colours.
- **Note names.** `Unit::Note` shows a MIDI note number with its name (`60 C4`)
  and accepts names (`C4`, `f#3`, `Db2`) when typed. Note Split's `low` and
  `high` use it.
- **Remove unused ports (#225).** Ctrl+double-click a node, or right-click it,
  to drop its unused ports in one undo step (`spare::remove_unused_ports`): a
  Mixer's inputs past the last wired one, and a group's (or its input or output
  node's) ports with no wire outside or inside. Other nodes are unaffected.

## Drum pads and node names

The Drum Rack template's slots are `Drum Pad` groups named for their drum (Kick, Snare, Closed Hat, Open Hat, Clap, Low Tom, High Tom, Crash). A pad owns its key (a one-output Note Split, set by the `key` input), a Choke Gate, a Sampler and a Pan, with `notes`, `key`, `level`, `tune`, `pan` and `choke in` inputs and `out` and `choke out` outputs. Dropping an audio clip from the timeline, or a single file from the desktop, on a pad (or on the Sampler) loads that drum, with no need to open the group. Choke is wires: a pad's `choke out` into any number of other pads' `choke in`; the two hats are wired to each other, and there is no graph loop because `choke out` is taken before the gate. The new **Choke Gate** node does the cutting. Event outputs of a group no longer get a meter stage, so notes can leave a group. Any node can now be named (F2 or double-click on its title; the `name` config); the group panel's Controls follow wires through nested groups, so `Kick level` still sets the Sampler's gain. The songs' racks are unchanged, so the golden renders are too. Tests: `choke_gate` (unit), `editor::tests` (pads and renaming), the `instruments` and `synced_effects` tests, route S09 (the hats choke each other). The S09 fixture loads the kit by pad name; dragging onto a pad is not routed yet.


## Sampler Fixed pick and pad `sample` / `take` inputs

The Sampler's `rr_mode` has a fourth choice, **Fixed**, with an `rr_pick` parameter (the slot, from 1, clamped to the slots there are): a set's copies of one drum are chosen by parameter, so the pick can be changed, automated or wired, and no ports change. Each Drum Pad gained a `sample` input (a sample or Sample Set wired over the Sampler's own; a dropped file still fills it when nothing is wired) and a `take` input (into `rr_pick`), and its Sampler is set to Fixed. A single-sample pad is unaffected, so no golden render moved. Prepares drum rack from clip (`docs/design/drum-rack-from-clip.md`, step B). Tests: `sampler` unit test, `realtime.rs`, `editor::tests`.

## Drum hit analysis (step A of drum rack from clip)

`noodle-io/src/drum_bins.rs` is the offline analysis behind the planned *Create drum rack from clip* (design: `docs/design/drum-rack-from-clip.md`; the command, session job and dialog are later steps). `analyse_hits(mono, rate, progress)` merges the full-band, bass and treble onset detectors (sensitivity 0.7; onsets within 10 ms are one hit), cuts each hit from 2 ms before its onset to the earliest of the next hit, a 50 dB fall and 1.5 s, and describes it by `HitFeatures` (24-band log-mel spectrum normalised to its loudest band, centroid, flatness, energy split below 150 Hz / to 5 kHz / above, decay to -30 dB, pitchedness and key from `PitchTracker`, number of attack bursts). A hit is flagged `truncated` (cut by the next hit while still within 30 dB of its peak) and `mixed` (another onset within 30 ms, or the previous sound still within 25 dB of its peak at its start). The progress callback returns `false` to cancel. `bin_hits(hits, merge)` does leader clustering of the unmixed hits and one medoid pass; `merge` (0 to 1) is the dialog's slider and re-bins from kept features. Mixed hits join a bin they fit but never start one; a hit that the sum of two bins' medoids explains better than one is marked mixed; quiet or truncated singletons and unfit mixed hits go to `other`. Members are ranked (clean, whole, loud, then nearest the medoid) and each bin gets a rule-based `DrumKind` guess with a confidence, `Unknown` under 0.4. `HitCache` keeps an analysis by file hash, frame window and `DETECTOR_VERSION`. Known limits: a hat quiet under a kick is not seen in the spectrum of the first 80 ms, and a drum that sounds with every hit of another has no clean example to bin; an onset at frame 0 is not found (the onset detector's limit). Tests are inline, on a synthetic kit; each was proved able to fail (loosened threshold, level normalisation removed, sum test removed, ring flag removed, truncation removed).

## Create drum rack from clip: the dialog (step D)

Right-click an audio clip, **Create drum rack from clip…** (`Edit::AnalyseDrumClip`). `drum_rack_dialog.rs` shows a progress bar with Cancel while the analysis runs, then the **groups** (bins: a name from the guess, hit count, confidence, the medoid's waveform) on the left and the **pads** on the right: the eight pads of the Drum Rack template, or all 128 keys with **Show all keys**, each named by its General MIDI drum name (`pad_name`). Each group is dropped on the pad its guess names (the first of a kind wins; Unknown groups stay unplaced). Drag a group onto a pad to place or replace it (one group may go on several pads); a pad whose group has several hits has a ◀ n/N ▶ picker for which one it plays (hits that are mixed or cut short are marked), × empties the pad. The **Merge more / Split more** slider re-bins from the kept features and re-guesses the pads. **Create** sends `Edit::CreateDrumRack` with the groups in use and the pads (one undo step) and closes; Cancel or Escape changes nothing. The user's choices live in the dialog; the session holds only the analysis. The dialog's two lists have a fixed height: a list that sizes itself to its content made the modal's layout change every frame under the test harness. Route: S09 `drum rack from a recorded drum clip` (right-click, wait, drag the group onto the kick pad, Create). The dialog has more: each group card has a drag handle (⠿), **▶ Play**, **Place on…** (a menu of the visible pads, the click path to placing), **Split** (`noodle_io::split_bin` tightens the threshold from the slider until the group falls apart, else says the hits are too alike), **Merge with…** (folds another group in; its pads follow) and a **Hits** list; each hit has a drag handle, ▶ and **Move to…** (any other group, a new one of its own, or Other), and a hit can be dragged onto any group. The Other list has the same rows, so a hit left out can be put back. Each pad has ▶ for the hit it plays. **Undo** takes back the last move, split or merge; the slider throws the manual edits away with the pads, as it did. Audition (`Session::audition_drum_hit`, `noodle_io::Preview`) plays the hit straight to the monitor device (the main output if none is chosen) through its own short stream, outside the engine and the graph, so the project is untouched; it is silent on the virtual output. (An earlier design played it through a copy of the project; Jeremy chose the bypass as the audition is never a permanent state.) `make_bin` and `split_bin` are the io helpers behind the edits. The drag sources are now small handles, not the whole card, because a button inside a drag source never gets the click. The S09 route places the group from its menu. A spectrogram on the card was dropped as not needed. Tests: `split_bin`, `make_bin` in `drum_bins.rs`; move, merge, split and undo of the dialog's state in `drum_rack_dialog.rs`.
