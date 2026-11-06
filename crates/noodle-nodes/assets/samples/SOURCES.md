# Shipped samples: sources and licences

These recordings are compiled into the program (`system_samples.rs`) so the
system library's sampled instruments play with nothing to install. Every one
is public domain (CC0 1.0 or the Unlicense), which allows copying, changing and
redistributing them, commercially or not, without attribution; the credits
below are given anyway. Each was converted for shipping: mono, 44.1 kHz,
16-bit, cut to the length used (a short fade at the end), and, for the body
percussion, scaled to a -1 dBFS peak. Nothing else was done to the audio.

`index.txt` says which file name is which content hash (ADR 0004); a test keeps
it in step.

| Files | Instrument | Source | Licence |
|---|---|---|---|
| `steel-drum-60` ... `steel-drum-81` (8) | Steel Drum | [jlearman.SteelDrum](https://github.com/sfzinstruments/jlearman.SteelDrum) at `dc15a36`, by Jeff Learman: a hand-made oil-drum steel drum, FLAC files `SteelDrum_0NN_*_4.flac` (velocity layer 4, first take) for C4, D#4, F#4, A4, C5, D#5, F#5 and A5 | The Unlicense (public domain) |
| `banjo-Ds3` ... `banjo-Cs5` (8) | Banjo | [ganjo](https://github.com/sfzinstruments/ganjo) at `ccff5cd`: samples of an SX six-string guitar banjo, recorded, edited and mapped by "itsclipping" (`Banjo_Common - *.wav`, first take) | CC0 1.0 |
| `body-*` (8) | Body Percussion Kit | [body_percussion](https://github.com/sfzinstruments/body_percussion) at `4ac9d89`, open-source body percussion (readme: "CC 0 license"; git author D. Smolken) (stomp, chest slap, handclap, finger snap, snap, belly, chest thump, cheek pop), loud layer, first round robin | CC0 1.0 |

The repositories are part of the [sfzinstruments](https://github.com/sfzinstruments)
collection of open-source sample libraries. Each states its licence in a
`LICENSE` file; the licence text of each is the CC0 1.0 legal code or the
Unlicense.

To add a sample: put the WAV here, add its name to `SAMPLES` in
`system_samples.rs`, run `UPDATE_SAMPLE_INDEX=1 cargo test -p noodle-nodes
system_samples`, and add a row above with the source and licence. Only public
domain or CC0 audio belongs here.
