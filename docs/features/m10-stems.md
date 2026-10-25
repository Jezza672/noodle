# M10: Stems

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** Batch 2 (Split to stems) is in code and works end to end with a fake separator, and downloads its weights (pinned Hugging Face export, MIT) and ONNX Runtime 1.22.0 (pinned GitHub release, MIT) on first use, after asking; the backend has not yet run the real HT-Demucs weights end to end, and there is no Intel Mac runtime. See the docs/architecture/ files (index: ../ARCHITECTURE.md) ("Stems"). Designed: CONTEXT.md, "Stems", and ADR 0006. Batch 1's result (PR #174) said Stem warp should not be built yet, and the research notes (`research/stem-warp/findings.md`) find no published gain from per-stem warping over whole-mix methods. Batch 3 is not started: it needs real mixes (vocals included) in `eval/clips.csv` and a run on the real HT-Demucs weights, neither of which the cloud can do.

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
