# Noodle drum hit classifier

A small MLP (36 inputs, two layers of 256, 9 outputs) that says what a detected drum hit is:
kick, snare, clap, closed_hat, open_hat, crash, ride, tom, other. Input is the feature vector of
Noodle's hit detector (`noodle_io::feature_vector`); normalisation is inside the graph. Outputs:
`probs` and `confidence` (the top probability). The app answers `Unknown` below 0.5 or for `other`.

Training: `tools/drum-classifier/train.py` on the dataset from `noodle-eval drum-dataset build`
(features measured from one-shots and synthetic loops; no audio is distributed).

## Data and licences

**This model is not licence-clean.** The training data is not required to be, and any real
recordings added to it (songs the maintainers labelled by hand) may carry any licence. Training
data (feature rows, audio, labels) is never uploaded with a model; only the model file and this
card are. Treat the model as a derivative of its training data and check before reusing it.

The synthetic part of the data:

Trained on features of the `drums-cc0` one-shot pack: Karoryfer Samples (Swirly, Big Rusty and
Unruly Drums, The Hat With The Phat), D. Smolken (Body Percussion) and Virtuosity Drums, all CC0 1.0;
and **MuldjordKit by Lars Muldjord, CC BY 4.0** (https://github.com/sfzinstruments/DrumGizmo.MuldjordKit,
mapped and converted by kinwie). Credit for the MuldjordKit must travel with the model and the dataset.
`manifest.json` of the dataset lists every sample with its licence and credit.

## Limits

About three quarters right on isolated one-shots of kits it has not heard, and much less on hits
that overlap other drums. Rides and crashes are hard to tell apart in a mix, and closed hats under
a kick are often called kicks. It has not been checked on electronic drums.
