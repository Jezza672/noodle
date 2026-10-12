# noodle-eval

Warp quality metrics for Noodle's dev tooling (ROADMAP M8, ADR 0003). Each
metric compares a *reference* with a *test* through a `TimeMap` that the caller
builds from the song's known source and target tempo maps.

```sh
cargo run -p noodle-eval -- compare ref.wav test.wav 1.5   # JSON; 1.5 = test is 1.5x as fast
cargo run -p noodle-eval -- validate manifest.csv
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
