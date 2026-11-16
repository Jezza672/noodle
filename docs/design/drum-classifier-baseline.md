# Rule labeller baseline (`test` split)

<!-- Generated; see the commands below. -->
Made with:

```sh
cargo run --release -p noodle-eval -- drum-dataset build --pack PACK --out DIR --loops 600 --seed 1
cargo run --release -p noodle-eval -- drum-dataset baseline --data DIR --out this-file.md
```

Dataset: the 549 one-shots of the `drums-cc0` pack, and 600 synthetic loops (seed 1): 32,139 detected hits,
of which 5,407 are in the test split (samples are split 70/15/15 by a hash of the file name, and a loop only
uses samples of its own split). The detector found 17,826 of the 30,514 hits the loops placed; 11,911 of its
detections matched no placed hit (cymbal tails, room, noise), and those are not counted in the tables.
Part of that is the generator being harsh (16th hats, bleed 12 to 30 dB down); the detector's own recall and
false positives are a separate question from labelling.

The current `guess` in `noodle-io/src/drum_bins.rs`, run on the features of every detected hit.
A ride is right when the rules say `crash` (the app has no ride); `other` is right when they say `Unknown`.
Hits quieter than -45 dB are left out (the app drops them), and so are `mixed` hits in the clean columns.

## One-shots, alone

81 hits: 16.0% right, 58.0% confidently wrong, 0 detections matched no placed hit.
Toms with a pitch: 2/9.

| truth \ guess | kick | snare | clap | closed_hat | open_hat | crash | ride | tom | other | Unknown | n | recall |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| kick | 6 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 8 | 75% |
| snare | 0 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 10 | 12 | 0% |
| clap | 0 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 100% |
| closed_hat | 0 | 1 | 2 | 2 | 1 | 0 | 0 | 0 | 0 | 0 | 6 | 33% |
| open_hat | 0 | 5 | 1 | 2 | 0 | 0 | 0 | 0 | 0 | 3 | 11 | 0% |
| crash | 0 | 3 | 1 | 1 | 1 | 0 | 0 | 0 | 0 | 3 | 9 | 0% |
| ride | 0 | 0 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 2 | 4 | 0% |
| tom | 2 | 2 | 2 | 0 | 0 | 0 | 0 | 1 | 0 | 2 | 9 | 11% |
| other | 4 | 7 | 5 | 1 | 1 | 0 | 0 | 0 | 0 | 2 | 20 | 10% |

## Loops, all hits

2960 hits: 26.7% right, 39.7% confidently wrong, 2194 detections matched no placed hit.
Toms with a pitch: 34/163.

| truth \ guess | kick | snare | clap | closed_hat | open_hat | crash | ride | tom | other | Unknown | n | recall |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| kick | 334 | 141 | 53 | 37 | 8 | 0 | 0 | 0 | 0 | 278 | 851 | 39% |
| snare | 10 | 121 | 79 | 27 | 3 | 0 | 0 | 0 | 0 | 318 | 558 | 22% |
| clap | 0 | 10 | 15 | 1 | 4 | 0 | 0 | 0 | 0 | 12 | 42 | 36% |
| closed_hat | 83 | 135 | 202 | 276 | 22 | 0 | 0 | 2 | 0 | 247 | 967 | 29% |
| open_hat | 2 | 28 | 8 | 9 | 2 | 0 | 0 | 0 | 0 | 21 | 70 | 3% |
| crash | 1 | 7 | 7 | 2 | 1 | 0 | 0 | 0 | 0 | 6 | 24 | 0% |
| ride | 2 | 57 | 63 | 6 | 8 | 0 | 0 | 0 | 0 | 60 | 196 | 0% |
| tom | 27 | 51 | 16 | 0 | 2 | 0 | 0 | 13 | 0 | 54 | 163 | 8% |
| other | 8 | 32 | 17 | 4 | 0 | 0 | 0 | 0 | 0 | 28 | 89 | 31% |

## Loops, clean hits only

280 hits: 37.9% right, 24.3% confidently wrong, 8 detections matched no placed hit.
Toms with a pitch: 2/6.

| truth \ guess | kick | snare | clap | closed_hat | open_hat | crash | ride | tom | other | Unknown | n | recall |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| kick | 80 | 21 | 5 | 0 | 1 | 0 | 0 | 0 | 0 | 40 | 147 | 54% |
| snare | 0 | 15 | 9 | 1 | 0 | 0 | 0 | 0 | 0 | 46 | 71 | 21% |
| clap | 0 | 1 | 2 | 0 | 2 | 0 | 0 | 0 | 0 | 3 | 8 | 25% |
| closed_hat | 3 | 4 | 13 | 7 | 2 | 0 | 0 | 0 | 0 | 17 | 46 | 15% |
| tom | 1 | 3 | 0 | 0 | 0 | 0 | 0 | 2 | 0 | 0 | 6 | 33% |
| other | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 0% |

