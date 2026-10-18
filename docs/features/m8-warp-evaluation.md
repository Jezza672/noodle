# M8: Warp evaluation

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** Built, apart from what needs a person. Batches 1 to 5 landed earlier (metrics crate, probes and harness, the five songs, borrowed truth, tempo detection in full mixes). The closing batch added the **CI gate** (`noodle-eval/tests/gate.rs`): Noodle's modes on the probes (x0.75, x1.5 and the step) must be no worse than the worst reference stretcher's numbers in `baseline/borrowed.json` on each gated cell, except the cells listed in `baseline/gate.json`, which are worse today and may not get worse (`UPDATE_GATE=1` regenerates the list when cells improve). Only physical checks gate (where onsets land, pitch, loudness); the Roberts and Paliwal features stay out until `noodle-eval validate` has been run on the listening-test dataset (IEEE DataPort, a login). The closing batch also fixed the known detection misses: the onset detector no longer fires dozens of times on a held chord (a block is compared with the average of the 30 ms before it), and `glass-ambient` now gets its tempo from its bells (`detect_treble_onsets`). `funk-engine`'s beat phase used to be half a beat off (its onsets favour the off-beat); it is now settled by a downbeat model that looks at where the harmony changes. **Needs a person:** CC0 clips for `eval/clips.csv` (`eval/fetch.sh --pin`), the IEEE DataPort download for `validate` and `fit`, and the macOS listening check.

A dev tool that scores the warp methods, so a change to Grain, Beats or Repitch
shows a before and after. Four batches:

1. **Metrics.** A `noodle-eval` crate (Rust) with the Roberts and Paliwal TSM
   features (phasiness, smoothed spectral similarity, peak delta, transient
   ratio, harmonic-percussive transient ratio) plus onset timing, pitch (f0),
   loudness and peak, and stereo width. Alignment uses the song's known source
   and target tempo maps, never Noodle's own warp map (which may come from a
   wrong detected tempo and would hide that error) and never an interpolated
   length. The metrics are validated against the published TSM
   listening-test dataset (CC BY 4.0, downloaded, not committed); one that does
   not correlate with the listeners does not gate CI.
2. **Probes and the harness.** Short controlled signals (click trains, a held
   chord, sweeps, noise bursts, a lone kick) and a harness that renders a
   project natively at a target tempo or tempo map (the *native render*), warps
   the same audio to it (headline: bare mix, detected clip tempo, automatic
   mode; also stems and fixed Grain and Beats), and scores both ways plus the
   round trip A to B to A. The warp under test is tempo conversion only, with no
   warp markers (no pinned transients, no quantise). Repitch is reported as the
   floor, not scored as a contender, because it cannot keep pitch. Tempo grid:
   constant ratios 0.5, 0.75, 0.9, 1.1, 1.5 and 2 (target tempo divided by
   source tempo, so 2 is twice as fast), plus a ramp, a step and a wobble from
   fixed seeds. Output is a metric table with a rank summary, diffed against a
   committed baseline of metric numbers (no audio), which shows a before and
   after and is never a threshold. Once the metrics are validated, CI gates on
   the probes (plus one small stand-in project if needed); a threshold is a
   distance from the reference that must stay no worse than the worst reference
   stretcher's on the same material, and is never set from Noodle's own
   current numbers. (Built: everything above except the gate and the stems.
   The reference stretchers it would be measured against arrive in batch 4.)
3. **Songs.** Five songs of 60 to 90 s with intro, verse, chorus, breakdown and
   drop, written as `.ron` projects through the normal commands (drums, plucks,
   pads, bass, arpeggios; synths and triggered one-shots). A 15 to 20 s excerpt
   of each joins the CI gate. Features a song needs
   that are missing get built if the song cannot exist without them, otherwise
   an issue is filed; the batch ends with the list of gaps found. (Built: the
   songs in `noodle-eval/src/song/`, their `.ron` projects in
   `noodle-eval/songs/`, the `songs` command and `baseline/songs.json`. Every
   instrument is a group node of primitive nodes. Nothing the songs needed was
   missing, so nothing was built for them; the drums are synthesised, since
   there is no sampler. Gaps found, filed as issues: tempo detection found nothing in
   any of the five mixes (#141, fixed in batch 5: it now reads the bass onsets too and finds four of the five within 0.2%, on the excerpt and on the whole song; `glass-ambient` first got none and now gets its tempo from the bells in the treble. The beat's phase is the most common position, and funk-engine's off-beat bass can put it half a beat off), the nodes have no tempo-synced echo or LFO
   (#142), and there is no sampler or drum rack (#143). The excerpts go through the harness in tests that
   check wiring only, like the probes'. Stems are scored from batch 5 on, with `songs --stems`.)
4. **Borrowed truth.** For audio we cannot generate: a fetch script for CC0
   speech, solo instrument and full-mix clips (pinned checksums), a gitignored
   `eval/local/` for any file dropped in, and a `noodle-eval borrowed` command
   that makes reference stretches with Rubber Band R3, Signalsmith and
   SoundTouch (whichever install), each stretcher's latency measured and
   removed before scoring and the audio cached outside git, with only metric
   numbers committed. There is no true time map here, so the map comes from the
   tempo case, the same one asked of every contender. Scores are relative to the
   references' spread, with reference-free checks beside them (onsets land where
   the tempo map says, pitch is kept against the source, loudness is kept); hard
   thresholds come only from the known-truth regime. The ramp, step and wobble
   cases need references that accept a time map: Rubber Band does
   (`--timemap`), SoundTouch and Signalsmith's simple API are constant-ratio, so
   varying tempo has one reference and no spread. (Built: all of it, with the
   one change that Rubber Band's R3 engine is constant-ratio only, because its
   3.3.0 offline R3 returns the wrong length with a time map; its R2 engine is
   the changing-tempo reference. `borrowed --probes` runs the references on the
   probes too, where the native render is known, and `baseline/borrowed.json`
   holds those rows: the reference numbers the CI gate will measure against.
   Not built: the clip list in `eval/clips.csv` is empty, because the build
   environment could not reach a host with CC0 audio to pick clips and pin
   their checksums; add rows, run `eval/fetch.sh --pin`, and paste the
   checksums.)

Out of scope: a learned quality predictor, and the authors' MATLAB evaluation
code.

**Done when:** one command prints the scores of every song, probe and reference
clip for each warp mode, and a change that smears drums or shifts pitch moves
a number and fails the CI excerpt.
