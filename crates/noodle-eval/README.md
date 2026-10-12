# noodle-eval

Warp quality metrics for Noodle's dev tooling (ROADMAP M8, ADR 0003). Each
metric compares a *reference* with a *test* through a `TimeMap` that the caller
builds from the song's known source and target tempo maps.

```sh
cargo run -p noodle-eval -- compare ref.wav test.wav 1.5   # JSON; 1.5 = test is 1.5x as fast
cargo run -p noodle-eval -- validate manifest.csv
```

## Metrics

| Group | Fields | Meaning |
|---|---|---|
| spectral | `log_distance_db`, `smoothed_band_distance_db`, `convergence` | Phase-blind distance between spectrograms, aligned by the map. 0 is identical. |
| phasiness | `phasiness_delta` | Loss of vertical phase coherence around spectral peaks, test minus reference. |
| transients | `peak_delta`, `transient_ratio`, `hp_transient_ratio` | Attack sharpness against the reference: 0 / 1 / 1 is a match, lower is smeared. |
| timing | `median_abs_offset_ms`, `max_abs_offset_ms`, `matched` of `expected`, `extra`, `drift_ms` | Where onsets land against where the map says, and slow drift. |
| pitch | `median_abs_cents`, `mean_cents`, `agreement` | YIN f0 against the reference; single-voice material only. |
| level | `lufs_delta`, `peak_delta_db` | Gated integrated loudness (BS.1770) and sample peak. |
| stereo | `correlation_delta`, `width_delta_db` | Left/right correlation and side-over-mid energy. |

The Roberts and Paliwal features are implemented from their papers, not from
the (unlicensed) reference code, so the details of weighting differ. That is
why `validate` exists: a metric that does not rank like the listeners does not
gate CI.

## Validating against listeners

The TSM listening-test dataset (Roberts and Paliwal, CC BY 4.0) is on IEEE
DataPort and needs a free login, so it is downloaded by hand and never
committed. Describe the pairs you want scored in a CSV:

```
reference,test,method,speed,mos
orig/a.wav,stretched/a_wsola_0.8.wav,wsola,0.8,3.4
```

`speed` is the factor the test was stretched by (output length is reference
length divided by speed); paths are relative to the CSV. `validate` prints each
metric's Spearman correlation with `mos`, pooled and averaged within each
speed (a pooled figure can come from the speed alone), and marks the metrics
whose absolute correlation is at least 0.5 both ways as usable for gating.
