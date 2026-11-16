# Real drum data for the drum classifier

Tools for building a labelled set of real-world drum hits from openly licensed audio, to train
and test the classifier beside the synthetic loops (`docs/features/drum-classifier.md`). Data
lives outside the repo, in a WORK folder (for example `~/Documents/Jezza/drum-real-work/data`).

1. `fetch_ccmixter.py WORK --tags funk,drums ...` downloads CC0 / CC BY / CC BY-SA uploads from
   ccMixter and records source, licence and credit in `WORK/manifest.csv`. Anything NC, ND or
   unclear is skipped by `fetch_ccmixter.py`. `label.py` also accepts songs you add yourself under any
   licence (write `all-rights-reserved`): they are training only, `stats` reports them, and the model
   card must say the model is not licence clean. Neither the audio, the features nor the labels of
   such songs are published.
   You can also add rows by hand (columns in `label.py`, `MANIFEST_COLUMNS`).
2. `label.py split WORK` keeps only the drums of full mixes (kind `mix`) with the app's HT-Demucs
   (`drum_real_split`; needs the model from the Models tab). Real drum multitracks and drum-only
   loops (kind `multitrack` / `stem`) skip this.
3. `label.py detect WORK` runs the app's hit detector (`drum_real_detect`, which calls
   `analyse_hits`) and writes each cut hit as a WAV.
4. `label.py label WORK` plays each hit and takes single keystrokes: class, then purity
   (`clean double truncated malformed mixed spurious`, as in the classifier). Space replays, `u`
   undoes, Tab skips, `q` quits; everything is saved as it goes (`labels.jsonl`, append-only).
   Each song gets a seeded sample of `--per-song` hits (default 150).
5. `label.py export WORK` writes `real_hits.csv` in the dataset generator's `hits.csv` columns
   (source `real:<song>`, split by a hash of the song, damage `real`).

Build the binaries with `cargo build --release -p noodle-eval --bin drum_real_detect --bin drum_real_split`.
Eval songs are never training data: do not put them in a manifest. Tests: `python3 -m unittest test_label`.
