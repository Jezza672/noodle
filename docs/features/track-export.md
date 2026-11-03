# Track export, loudness and dither

Delivered. Closes #291. This is roadmap batches 1 and 3 of "Track export and render to clip"; render to clip (batch 2) is below. The design is in [architecture/caching.md](../architecture/caching.md) (Export, Track export, Loudness and dither).

**Status:** done.

1. **Track export.** The export dialog lists the mixdown and every top-level group (tracks and buses) as tick boxes. The mixdown is the existing render through the Outputs. The ticked groups are written by one `render_taps` pass, so the files have matching lengths and the same renders behind them. A track file is the signal at the group's output stage: after the group's gain, mute and solo, before whatever the group feeds, including the master processing. With the mixer going straight to an Output the track files sum to the mixdown (`noodle-nodes/tests/export.rs`). More than one file goes in a folder, named for the groups (`session/export.rs`, `unique_file_name` cleans and de-duplicates).
2. **Loudness.** The BS.1770 integrated loudness moved from `noodle-eval/src/level.rs` to `noodle_io::LoudnessMeter`, which measures chunk by chunk, and gained true peak (4× oversampled). `noodle-eval` uses the same code. Every file is measured as it is written; the dialog shows integrated loudness and true peak when the export ends. An optional target normalises the mixdown, by a gain that the track files share. `noodle-eval/tests/export_loudness.rs` checks the export's figure against the evaluator's on the written file for every example song.
3. **Dither.** `ExportWriter` adds TPDF dither when it rounds to 16 or 24 bits (WAV and FLAC), from a fixed seed, leaving exact silence silent. 32-bit float is untouched.
4. **Not done.** A mixer's own per-input gain and mute are not in a track file (only the group's output gain and the track's mute are). Nested groups can't be ticked. No MP3. No album sequence with gaps and crossfades, which waits on the setlist design (#366); no DDP image (dropped for the CUE sheet below).

## True-peak ceiling, ISRC and CUE sheet

Delivered. Closes #360 (album sequencing excepted, see above).

1. **True-peak ceiling.** The export dialog's "Limit the mixdown to −1 dBTP" puts `noodle_io::TruePeakLimiter` (`limiter.rs`) after the normalising gain. It is a lookahead limiter that reads the 4× oversampled peak the loudness meter measures, takes the smallest gain any frame in a 5 ms window needs and smooths it with a 5 ms moving average, so the gain falls before a peak and never exceeds what a frame needs. It runs in the writer (offline, like dither), is not a node, and delays the audio by its latency, flushed at the end so no frame is lost. When it is on together with a loudness target the export measures the limited result in an extra pass and makes up the loudness the limiter took, once, so the file lands at or just under the target. The loudness and true peak the dialog reports are those of the file as written. **Only the mixdown is limited**: track files keep the shared gain, so they still sum to the mixdown only when the ceiling is off or never engages. `ExportRequest::true_peak_ceiling`.
2. **ISRC and tags.** Title, artist and ISRC fields (`ExportMetadata`) are written into the mixdown: a WAV `LIST/INFO` chunk (`INAM`, `IART`, `ISRC`, appended after the data) or a FLAC `VORBIS_COMMENT` block (`TITLE`, `ARTIST`, `ISRC`). `normalize_isrc` accepts an ISRC with or without dashes (`US-RC1-76-07839`) and refuses a malformed one; the dialog disables Export and says why.
3. **CUE sheet.** "Write one next to the mixdown" writes `<mixdown>.cue` with the title, artist, ISRC and a single track starting at 00:00:00 (`cue_sheet`). A CD wants WAV 16-bit at 44.1 kHz; the dialog does not force it. With sections (#329) a track per section would follow.
4. **Tests.** `noodle-io/src/limiter.rs` (the true peak ends under the ceiling, mutation-checked against a limiter that looks only at sample peaks), `export.rs` (tags read back in WAV and FLAC, CUE text, ISRC checks), `noodle-nodes/tests/export.rs` (ceiling with and without a target, tracks not limited, tag and CUE files).

## Render to clip

Delivered. Closes #292.

A track's header menu has **Render to clip** (the whole timeline) and **Render loop to clip** (the transport loop's span, when it is on). The group is rendered by the same one-pass `render_taps` that writes a track file (`noodle-nodes/src/render_clip.rs`), so the clip is the group's own signal after its gain and mute, offline and frozen nodes included. The render starts at the beginning of the timeline, so stateful nodes are where they would be; the range only chooses what is kept.

- **Tail.** The render runs past the range's end and the file stops after the last sample above -80 dB (`CLIP_SILENCE`), no later than 10 s past the end (`MAX_TAIL_SECONDS`). A tail still sounding at the cap fades out over 2048 frames. The status line says when that happened.
- **File.** A 32-bit float WAV in `<project> renders/` next to the project file, named for the group (de-duplicated against what is there). The clip refers to it by its path relative to the project, like any audio clip (ADR 0004).
- **One undo step.** A `Command::Batch` adds a track named `<name> (render)` right under the original, puts the clip on it at the range's start, and mutes the original (kept). The track is an ordinary audio track, so the clip can be cut and warped. `Session::render_to_clip`, `session/render_clip.rs`.
- **Refused.** An unsaved project (no folder to write to), a group with no output, and a silent render (nothing is written). Cancelling leaves no file.
- **Tests.** `noodle-nodes/tests/export.rs`: the clip equals the track export's samples, the tail stops at silence or at the cap, a silent track makes no file. `session/render_clip.rs`: the new track, the clip, the muted original, one undo and redo, a range.

## Export tail

Delivered. Closes #361.

The export dialog has a **Tail** row: **To silence, up to N s** (default, 10 s) or **Fixed N s**. It is one length for every file of the export, whole project or range, and it is saved with the project (`ExportSettings` in `noodle-core`, set by `Command::SetExportSettings`, so changing it is one undo step and it is not written while it is the default).

- **Fixed** extends `end` by the seconds, as the old fixed four seconds did, and now also for a range export (which had no tail).
- **To silence** sets `ExportRequest::silent_tail` to the cap in frames. The render runs `cap` frames past `end`; each file writes the body straight through and holds the tail in memory (at most the cap, per file). When the pass is done the tail is cut at the latest frame any file (the mixdown and every stem) is still above -80 dB (`CLIP_SILENCE`), so all files stay the same length and line up at the start. A tail still sounding at the cap fades out over 2048 frames in every file. The loudness figures are measured on what is written. The body is always kept whole, as in render to clip.
- Tests: `noodle-nodes/tests/export.rs` (equal lengths across mixdown and stems, cut at the last sounding frame, capped tail fades), `noodle-core` (saved and undone).
