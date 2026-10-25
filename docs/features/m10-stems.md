# M10: Stems

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** Batch 1 (the ceiling experiment) is done: a trade, not a clear gain (result below). Batch 2 (Split to stems) is in code and works end to end with a fake separator, and downloads its weights (pinned Hugging Face export, MIT) and ONNX Runtime 1.22.0 (pinned GitHub release, MIT) on first use, after asking; the backend has not yet run the real HT-Demucs weights end to end, and there is no Intel Mac runtime. See the docs/architecture/ files (index: ../ARCHITECTURE.md) ("Stems"). Designed: CONTEXT.md, "Stems", and ADR 0006. Batch 1's result (PR #174) said Stem warp should not be built yet, and the research notes (`research/stem-warp/findings.md`) find no published gain from per-stem warping over whole-mix methods. Batch 3 is not started: it needs real mixes (vocals included) in `eval/clips.csv` and a run on the real HT-Demucs weights, neither of which the cloud can do.

AI stem separation (HT-Demucs through ONNX Runtime), used two ways. Research and the
sources are in the project folder `research/stem-warp/findings.md`. Three batches:

1. **Ceiling experiment.** Before any model. `songs --stems` is not enough: `Harness::run_stems`
   scores each part alone against that part's native render, and the song's parts are
   instruments (Kick, Clap, ...), not drums, bass, vocals and other. This batch adds a mapping
   from parts to stems and a new harness mode that warps each stem (Beats for drums, Grain for
   the rest), adds the results and scores the sum against the native mix, next to the
   whole-mix modes and a harmonic/percussive baseline. With perfect stems the residual is zero,
   so it also adds simulated leakage between stems to test the residual. The generated songs
   have no vocals, so the vocals default is untested by this batch. If there is no clear gain,
   stop here. The ADR and docs landed in #166.
   **Result** (`noodle-eval songs --ceiling`, five songs, all nine tempo cases, baseline in
   `baseline/ceiling.json`; `songs --verdict` reads it again and has the per-metric table). The
   stems are exact: drums, bass and other (the songs have no vocals), rendered alone through the
   mix's master chain. They add back to the mix to -70 dB or better on three songs; on
   breakbeat-run and funk-engine the master bus compression leaves a residual 14 dB under the
   mix, which is warped in Grain with the rest. Each stem warps at the true source tempo; the
   default modes are Beats for the drums and Grain for the rest. Native view, median error over
   Auto's, per metric (below 1 is better):
   - Spectral fidelity improves: `log_distance_db` 0.88 with perfect stems, 0.87 at 20 dB of
     leakage and 0.99 at 10 dB; `mfcc_delta_dist` 0.98; `smoothed_band_distance_db` 0.97.
   - Transients get worse, and more with leakage: `flux_peakiness_abs_ln` 2.22 (perfect), 3.88
     (20 dB), 3.66 (10 dB); `HPSTrRat_abs_ln` 1.45; `percussive_share_abs_ln` 1.14. Phase
     (`SPhMW`, `MPhMW`) and timing are level.
   - Pooled over all metrics the median is 1.00 against Auto and Beats alone and 0.98 against
     Grain alone (1.02 against the best of the three per metric, an oracle). 30 / 20 / 10 dB of
     leakage pool to 1.03 / 1.04 / 1.13 against that oracle; the harmonic/percussive split with
     no model, 1.21.
   - The control for mixing modes: exact stems all in Beats pool to 1.000 against that oracle
     (0.986 against Auto, 0.993 against Beats), with `log_distance_db` 0.82 (better on every
     cell) and transients closer (peakiness 1.18). All in Grain are worse (1.07). So the default
     per-stem modes cost something, and Grain on the "other" stem is the part to doubt; a
     per-stem mode choice is the one thing that could still make a gain, and it needs a design
     of its own.
   - By song (stems against the best whole-mix mode): rainy-lofi 0.94, glass-ambient 1.00,
     neon-drive 1.14, funk-engine 1.04, breakbeat-run 1.09. Only rainy-lofi gains.

   **Decision for the owner:** the gain is a few percent at best, depends on the song, and the
   transient cost grows with any leakage, so do not build Stem warp (batch 3) as designed; it
   would cost HT-Demucs, a downloaded model and a native runtime. Split to stems (batch 2) stands
   on its own as an editing feature. Caveats: the songs have no vocals, flat leakage is a simple
   stand-in for a real model's errors, and a real model's output was not tried.
2. **Separation and Split to stems (in code; remaining: a built-in weights address once the licence is checked, and shipping or fetching ONNX Runtime).** The separator trait in its own crate (added to
   CLAUDE.md's crate table), with a fake separator for tests, the ONNX backend, the consented weight
   download, a cancellable background job with progress, and **Split to stems** on an audio
   clip: one new track under the source for each stem and for the residual, named
   "<clip> (drums)" and so on, stems written as WAVs in a project-relative folder, all one undo
   step. Audio clips have no mute today (`AudioClip` has no such field), so this batch adds clip
   mute, and the source clip stays in place, muted. Each new clip gets its own new warp copied
   from the source's clip tempo, beat markers and warp settings (never the source's `WarpId`,
   which would link them).
3. **Stem warp mode.** A warp mode alongside Repitch, Grain and Beats. Choosing it separates in
   the background into a hidden cache (clip content, model id and version) in the existing
   per-user `CacheStore`, plays as Repitch until ready, then warps each stem with a fixed mode
   (drums Beats; bass, vocals, other and the residual Grain) and adds them back. A change of model
   or version acts like a `DSP_GENERATION` bump. Splitting afterwards copies from the cache. A
   per-stem mode picker comes only if the evaluation shows the defaults are wrong. Prerequisite:
   `eval/clips.csv` gets real mixes, vocals included, because the generated songs have none and
   M8's borrowed-truth set is empty. It ships only if the evaluation shows a clear gain on most
   songs and those clips.

Out of scope for now: streaming separation, models other than HT-Demucs, and note-level
warping. The note-level idea (neural note onsets placing Beats cuts inside a stem) is a later
step, after Stem warp; true per-note audio extraction is not planned.
