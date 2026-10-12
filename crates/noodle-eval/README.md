# noodle-eval

Warp quality metrics for Noodle's dev tooling (ROADMAP M8, ADR 0003). Each
metric compares a *reference* with a *test* through a `TimeMap` that the caller
builds from the song's known source and target tempo maps.

```sh
cargo run -p noodle-eval -- compare ref.wav test.wav 1.5   # JSON; 1.5 = test is 1.5x as fast
cargo run -p noodle-eval -- validate manifest.csv
cargo run -p noodle-eval -- probes                         # the probe grid, all methods
cargo run -p noodle-eval -- probes --probe held-chord --tempo x1.5 --method grain
```

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

## Probes and the harness

`probes` warps short controlled signals (`probe.rs`) through the real engine
(`harness.rs`) and prints their scores. Nothing here fails a build because a
warp got worse: the committed baseline is a before and after, and no metric
gates CI until it has passed `validate` (ADR 0003).

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
