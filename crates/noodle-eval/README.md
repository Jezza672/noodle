# noodle-eval

Warp quality metrics for Noodle's dev tooling (ROADMAP M8, ADR 0003). Each
metric compares a *reference* with a *test* through a `TimeMap` that the caller
builds from the song's known source and target tempo maps.

```sh
cargo run -p noodle-eval -- compare ref.wav test.wav 1.5   # JSON; 1.5 = test is 1.5x as fast
cargo run -p noodle-eval -- validate manifest.csv
cargo run -p noodle-eval -- fit manifest.csv --cache f.csv # a score fitted to listeners from all the metrics
cargo run -p noodle-eval -- probes                         # the probe grid, all methods
cargo run -p noodle-eval -- probes --probe held-chord --tempo x1.5 --method grain --export wavs
cargo run -p noodle-eval -- songs                          # the five songs' excerpts, all methods
cargo run -p noodle-eval -- songs --song neon-drive --tempo x1.5 --full --export wavs
cargo run -p noodle-eval -- borrowed --probes              # vs Rubber Band, SoundTouch, Signalsmith
cargo run -p noodle-eval -- borrowed --clip my-speech      # a file in eval/local
```

`--export DIR` writes every render as a WAV to listen to: `<probe>_source.wav`,
`<probe>_<tempo>_native.wav` (the answer key), and per method
`<probe>_<tempo>_<method>_warped.wav` and `_roundtrip.wav`. The full grid is
about 1 GB, so pick a probe or tempo.

## Metrics

Before any of these, the test is gain-matched to the reference by integrated
loudness, so a pure gain change moves only the `level` group.

**The paper's features** (`paper.rs`, in `Report::paper`), as Roberts and
Paliwal define them (JASA 2021, arXiv:2006.06153, section II), under their
names. The paper aligns by interpolating to a common length; here the caller's
`TimeMap` does it. The module docs list every detail the paper leaves open and
what was chosen.

| Field | Paper's name | Ideal |
|---|---|---|
| `mph_mw`, `sph_mw` (`_nw` unweighted) | MPhMW, SPhMW: horizontal phase difference over time | 0 |
| `ssmad`, `ssmd` | Spectral similarity: cubic fit to each frame's normalised magnitude spectrum, intercept removed | 0 |
| `delta_p` | ΔP: onsets per second, test minus reference | 0 |
| `tr_rat` | TrRat: mean onset height, reference over test (`NaN` if a signal has no onsets above mean plus deviation) | 1 |
| `hps_tr_rat` | HPSTrRat: RMS of the Driedger transient signal, reference over test | 1 |

**Noodle's own diagnostics**, which say more about what went wrong. They are
not the paper's measures.

| Group | Fields | Meaning |
|---|---|---|
| spectral | `log_distance_db`, `smoothed_band_distance_db`, `convergence` | Phase-blind distance between spectrograms, aligned by the map. 0 is identical. |
| vertical phasiness | `vertical_phasiness_delta` | Phase spread across the bins around each spectral peak (one signal), test minus reference. |
| transients | `flux_peak_change`, `flux_peakiness_ratio`, `percussive_share_ratio` | Attack sharpness against the reference: 0 / 1 / 1 is a match, lower is smeared. |
| timing | `median_abs_offset_ms`, `max_abs_offset_ms`, `mean_offset_ms`, `matched` of `expected`, `extra`, `drift_ms` | Where onsets land against where the map says; `drift_ms` is the spread of the lag over the piece, so a constant delay is 0 there and shows in `mean_offset_ms`. |
| pitch | `median_abs_cents`, `median_cents`, `agreement` | YIN f0 against the reference; single-voice material only. |
| level | `lufs_delta`, `peak_delta_db` | Gated integrated loudness (BS.1770) and sample peak. |
| stereo | `correlation_delta`, `width_delta_db` | Left/right correlation and side-over-mid energy. |

A `TimeMap::Points` map is piecewise linear, so a smooth tempo ramp has to be
sampled densely by whoever builds it, and the points must increase in both
times.

## Validating against listeners

The TSM listening-test dataset (Roberts and Paliwal, CC BY 4.0) is on IEEE
DataPort and needs a free login, so it is downloaded by hand and never
committed. Describe the pairs you want scored in a CSV:

```
reference,test,method,speed,mos
orig/a.wav,stretched/a_wsola_0.8.wav,wsola,0.8,3.4
```

`speed` is the factor the test was stretched by (output length is reference
length divided by speed); paths are relative to the CSV. This is the inverse
of the paper's `β` (output length over input length), which is what the
dataset's file names carry, so invert it when building the manifest from
them. Paths are relative to the CSV. `validate` prints each
metric's Spearman correlation with `mos`, pooled and averaged within each
speed (a pooled figure can come from the speed alone), and marks the metrics
whose absolute correlation is at least 0.5 both ways as usable for gating.

## Fitting a score

No single metric tracks listeners closely (on the dataset every one sat below
|r| 0.4 within a speed, and the paper itself reports no per-feature figure and
notes the lack of individually correlated features). Its predictor combines
many features. `fit` does the same on the same manifest: a ridge regression
over every metric, checked on pairs whose *reference clip* the model never
saw (5-fold, grouped by clip), reported as held-out Pearson, Spearman and RMSE
against guessing the mean, per method, with the weights. `--cache FILE` keeps
the measured metrics (the slow part, which now uses every core) so a rerun only
refits; `--out MODEL.json` saves the model.

## Probes and the harness

`probes` warps short controlled signals (`probe.rs`) through the real engine
(`harness.rs`) and prints their scores. The committed baseline is a before and
after and never fails a build. The one gate is `tests/gate.rs`: on the probes
(x0.75, x1.5, step) Noodle's physical checks (onset timing and drift, pitch,
loudness) must be no worse than the worst reference stretcher's numbers (Rubber Band R2 alone on the changing cases) in
`baseline/borrowed.json`, except the cells listed in `baseline/gate.json`, which
are worse today and may not get worse. `UPDATE_GATE=1 cargo test -p noodle-eval
--test gate` rewrites that list after a change that improves cells. The paper's
features do not gate until they have passed `validate` (ADR 0003).

**Probes** are written in beats and rendered *natively* at any tempo map, so
the same probe at the target tempo is the answer key for a warped one. Sounds
keep their length in seconds; only where they start follows the tempo.

| Probe | Shows |
|---|---|
| `click-train` | timing, and whether clicks stay sharp |
| `lone-kick` | pre-echo and smearing around one attack |
| `held-chord` | phasiness on sustained, tonal sound |
| `sweep` | following a moving partial; pitch (one voice). A moving partial turns timing into pitch (at 180 bpm it climbs about 1800 cents/s, so 18 ms of offset reads as about 30 cents): read its pitch with its `offset_ms` |
| `noise-bursts` | transients in noise, and the stereo image |

**Tempo grid** (`tempo.rs`): the source is steady at 110 bpm (the CLI's; the tempo detector's range is 80 to 160, and not 120 so a fallback to the default tempo cannot pass for a right answer) and the target is
a constant ratio (`x0.5`, `x0.75`, `x0.9`, `x1.1`, `x1.5`, `x2`; target tempo
over source tempo) or one of `ramp` (0.75 to 1.5 times), `step` (0.8 then 1.25
times, at the middle) and `wobble` (a fixed seed). Noodle's tempo map steps, so
a ramp and a wobble are quarter-beat steps.

**Methods**: `auto` is the headline: the clip tempo and mode the app would
choose from the bare audio (`noodle_io::Analysis`: Beats for close
transients, Grain for the rest), so wrong or missing detection is scored (with no tempo found the clip stays at the timeline's tempo, as in the app). `grain`,
`beats` and `repitch` use the true clip tempo. Repitch is the floor, not a
contender: it cannot keep pitch. The warp is tempo conversion alone (no warp
markers, no quantise).

**Views**: each warp is scored three ways, always through a time map built from
the known tempo maps and never Noodle's own warp map.

- `native`: against the native render at the target tempo. Known truth.
- `source`: against the source, through the map from target time to source
  time (the setup of the listening-test dataset).
- `round-trip`: warped to the target and back to the source tempo, against the
  source. Repitch passes this by construction, so it never stands alone.

The output is a table of headline metrics, a mean rank per contender (every
metric counts once, so read it as "better on the whole", not "better on what
matters"), and a diff against `baseline/probes.json`. `--update` writes the
rows that ran into the baseline (numbers only, no audio, rounded to 6
places); a metric that moved by more than 1% shows in the diff.

## Songs

`songs` does for the generated songs what `probes` does for probe signals:
the song at its own tempo is the source, the same notes rendered at the target
tempo map by the engine are the native render (known truth), and every method
is scored in the same three views. The warp is tempo conversion alone, as for
probes, and `auto` is still the headline.

| Song | bpm | Length | What it is for |
|---|---|---|---|
| `neon-drive` | 126 | 69 s | Four on the floor: steady kick and offbeat hats, saw plucks, arpeggio, pad and lead |
| `rainy-lofi` | 88 | 87 s | Sparse, soft drums with ghost notes, round bass and keys, mellow pad |
| `breakbeat-run` | 138 | 70 s | Dense syncopated breakbeat, sixteenth hats, growling bass, stabs |
| `glass-ambient` | 100 | 86 s | Almost no drums: bell plucks, long pads, a deep sine bass |
| `funk-engine` | 110 | 70 s | Syncopated plucked bass, ghosted snares, clav stabs |

Each has an intro, verse, chorus, breakdown and drop. They are written in
`src/song/` as parts and patterns in beats (`music.rs` documents the little
pattern language), then built into a project through the normal commands: a
track per part, and each instrument (`kit.rs`) a group node made of oscillators,
noise, filters, envelopes, VCAs and Math, with its controls as group inputs and
everything editable. `songs/*.ron` are those projects at the song's own tempo
(`noodle render songs/neon-drive.ron out.wav` plays one; `UPDATE_SONGS=1 cargo
test -p noodle-eval --test songs` rewrites them after a change to the songs, and
the test fails while they are stale). The drums are a drum rack of Samplers
playing one-shots rendered from the synthesised drums and committed in
`songs/samples` (named by content hash, `index.txt` says which is which; the
same command rewrites them, and a test checks they still match the synthesis).
The mixes run through Dynamics (compressor, limiter), EQ and Reverb nodes, with
a 30 Hz low cut and a whole-mix compressor on funk-engine and breakbeat-run too:
the songs are benchmarks, so a detector that fails on a processed mix is fixed,
not the mix softened (#156; `the_tempo_survives_a_low_cut_and_bus_compression`
in `noodle-io/src/onsets.rs` is its regression test).

The default run takes each song's **excerpt** (15 to 20 s, the busiest stretch,
which is what joins the CI tests); `--full` takes the whole song and names its
rows `NAME-full`. The committed baseline is `baseline/songs.json`, the excerpts
over the whole tempo grid and every method; like the probes' it is a before and
after, not a threshold.

`--stems` scores each part of a song (kick, bass, pads and so on) on its own
instead of the mix, as rows `NAME/PART`: a stem is analysed alone, so its
detected tempo is what the app would find on that instrument exported as a file.
Run it with `--baseline` to diff against a file of your own, or `--update` to add
the rows to the committed baseline.

`--ceiling` is the M10 perfect-stem experiment (ROADMAP M10, ADR 0006): what Stem warp could
score if the separator were flawless. The song's parts are mapped to stems (`stems.rs`:
drums, bass, other; the songs have no vocals) and rendered alone through the mix's master
chain, and what they do not add up to is the residual. `stems` warps each stem (Beats for
drums, Grain for the rest, residual Grain) at the true source tempo and adds them up;
`stems-beats` and `stems-grain` warp the same stems all in one mode (the cost of splitting without a mode per stem); `stems-30`, `stems-20` and `stems-10` first bleed that many dB of the other stems into each
(the sum is unchanged, only where the sound sits); `hpss` is the model-free baseline, the
mix split into harmonic (Grain) and percussive (Beats) parts by median filtering. They are
scored against the native mix next to `auto`, `grain` and `beats`, and the run ends with a
verdict: how often and by how much each split contender beats the best whole-mix mode per
metric, Auto, Grain and Beats alone, and a table per metric (read it: a pooled median can net
spectral gains against transient losses to 1). The grid takes about an hour on four cores;
`songs --verdict` prints the verdict again from `baseline/ceiling.json`.

Things to know when reading a song's numbers:

- Where a note starts and how long its gate is follow the tempo; the sounds'
  own times stay in seconds (envelope decay and release, a kick's pitch sweep),
  as a synth's do. That difference is the irreducible error of the answer key.
  The songs use no echo or LFO, whose time and rate would not follow the tempo
  either: no stretcher could reproduce them (#142).
- On dense material the timing metric pairs an onset with the nearest onset
  within its window, and a song has about eight onsets a second, so a wrong
  time map still pairs most of them (median offset about 20 ms at 1.2 times for
  a true 1.5). Read it next to `drift_ms`; probes show timing sharply.
- Pitch is meaningless on a full mix (`agreement` is low), as the metric docs
  say, and shows only for single voices.

## Borrowed truth

`borrowed` is for audio we cannot generate: speech, a solo instrument, a
recorded mix. There is no native render of those, so Noodle's warps are
measured against what established stretchers make of the same audio
(`borrowed.rs`, `refs.rs`; ADR 0003).

**Clips.** `eval/local/` holds any audio file you drop in (WAV, FLAC, MP3, Ogg;
git ignores it), and `eval/fetch.sh` downloads the CC0 clips listed in
`eval/clips.csv` into `eval/cache/clips/`, refusing any file whose SHA-256 is
not the pinned one (`--pin` prints a new row's checksum on first download).
`--clip NAME` picks one by file name, `--seconds N` cuts each to its first N
seconds (default 30), and `--probes` adds the probe signals, which also have a
native render, so they are scored in both views. Every piece is stereo at 48 kHz
and rounded to 16 bits, so all contenders are given the same samples.

**References.** Whichever of these are installed:

| Name | Program | Notes |
|---|---|---|
| `rubberband-r3` | `rubberband -3` (Rubber Band 3.0+) | Constant ratios only: in 3.3.0 offline R3 with `--timemap` warns of a NaN ratio and returns the wrong length. |
| `rubberband-r2` | `rubberband -2` | Also follows a time map, so it is the one reference for the ramp, step and wobble cases. |
| `soundtouch` | `soundstretch` | Constant ratios only. |
| `signalsmith` | `eval/build-signalsmith.sh` builds it into `eval/cache/bin` | Constant ratios only. Its source revisions are pinned in the script. |

A reference that is missing is skipped with a note on stderr; `--tool NAME`
limits the set. A stretch whose length is not the plan's is refused and
reported, not scored.

**The map.** There is no true time map for borrowed audio, so the map comes
from the tempo case: each piece is declared to be at 110 bpm and every contender
(each reference and each Noodle mode) is asked for the same map, never Noodle's
own. Noodle's `auto` still picks its tempo from the audio, so a wrong detection
shows as a score.

**Delay.** Every stretcher returns its audio late or early by an amount of its
own, and it changes with the ratio (SoundTouch's reaches 55 ms at half speed).
For each stretcher and case, the same plan is applied to a calibration signal
of noise bursts at known times, the delay is the median distance of the
bursts from where the map puts them, and that delay is removed before scoring.
`--tables` prints the delays removed; a delay that is not constant over the
signal is noted. `--no-latency-compensation` scores the audio as returned.

**Cache.** Stretches are kept in `eval/cache/refs/`, keyed by program, a
fingerprint of the binary and its arguments, the audio, the case and the plan
(its map and lengths); delete the folder (or
change `--cache`) to make them again. Only metric numbers are committed:
`baseline/borrowed.json` holds the probes' rows, keyed `piece/case/contender/view`,
and shows a before and after like the others. Its reference numbers came from
Rubber Band 3.3.0, SoundTouch 2.3.2 and Signalsmith Stretch at the revisions in
the build script; a different version of a reference can move them.

**Reading the output.** For each metric, the references' lowest and highest
value is the range a good stretcher falls in. The summary counts, per Noodle
mode, how often it was better than (`<`), inside (`=`) or worse than (`>`) that
range over every piece, case and view; with `--tables` each case is printed with
the marks. Values within 2% of the range's edge count as inside. A single
reference has no spread, only a point to compare with, and the ramp, step and
wobble cases have just `rubberband-r2`. The floor, `noodle-repitch`, is shown
but not counted.

Next to those are the checks that need no reference stretcher: the median onset
offset and missed onsets (do the onsets land where the map says), drift, pitch
against the source (single voices only) and loudness change. They are printed
for every contender, references included.

Nothing here fails because a number is bad. A hard threshold comes only from
the known-truth regime (ADR 0003), where the references' scores on the probes
and songs say how much a method may differ from the answer.
