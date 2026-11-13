# Sample packs: publishing

The design is ADR 0012; the words are in CONTEXT.md ("Sample packs"). This file
covers what is built: the pack definition, the site `noodle-publish` builds, and
the CI workflow that uploads it. The downloader and the Sounds panel are not
built yet.

## Layout in git

```text
packs/<pack id>/pack.ron      a `noodle_packs::PackDef`
packs/<pack id>/<file>.wav    the audio each sample's `file` names
```

A `file` is relative to the pack's directory. Each sample's `hash` is
`Sample::hash()` of its decoded audio, so a pack file that is edited, or
re-exported at another depth, must have its hash updated: both `validate` and
`build` refuse a file whose hash differs and print the one it has.

**Open question: where the audio lives for CI.** ADR 0012 keeps audio out of git,
but `build` needs the files. Until that is decided the workflow builds what is
checked out under `packs/`, so audio has to be committed (or fetched by a step
added to the workflow, for example from Git LFS) for a real publish.

## The site

```text
index.json                      noodle_packs::Index, packs sorted by id
packs/<pack id>/manifest.json   noodle_packs::Manifest, author and licence resolved per sample, `bytes` filled in
objects/<hash>.flac             one immutable object per sample
```

`noodle-publish` builds it. An object is 24-bit FLAC of the sample as
`Sample::to_audio` gives it (mono stays mono, anything wider than stereo is cut
to two channels). Object names are the hash, so a changed sample is a new object
and an old one is never removed.

- **Lossless is checked, not assumed.** FLAC holds integers, so a sample is
  refused (`NotLossless`) unless every value is exactly a 24-bit integer scaled
  by 2^-23, which is what a 16 or 24-bit source file decodes to. Then the new
  file is decoded again and its hash must equal the object name before it is
  renamed into place (`RoundTrip`). A float file with other values is converted
  by the author first.
- **Idempotent.** An object already in `--out` is left alone. Writes go to a
  `.partial` file and are renamed, so an interrupted build leaves nothing a
  reader or `verify` could take for an object.
- **Last block.** The encoder records a shorter final block as the smallest, and
  Symphonia then fails to open the file; the stream info is rewritten with equal
  minimum and maximum, as `noodle-io`'s `ExportWriter` does.

## Commands

```sh
noodle packs validate packs              # definitions, files, hashes; writes nothing
noodle packs build packs --out site      # the site above
noodle packs verify site                 # every object decodes to the hash in its name
```

Tests: `crates/noodle-publish/tests/publish.rs` builds from generated WAVs, then
checks the objects decode to the same audio, a rebuild writes nothing, wrong
hashes, lossy audio, missing files and duplicate ids are refused, and `verify`
catches a flipped byte and a missing object.

## CI: `.github/workflows/packs.yml`

- **Pull request** touching `packs/**` or `noodle-publish`: `packs validate`.
- **Push to main** touching the same paths: `packs build`, `packs verify`, then
  upload to R2 with the AWS CLI against the bucket's S3 endpoint. The upload
  goes objects first (`sync --size-only`, never `--delete`), then manifests, then
  `index.json` last, so a reader never sees a pack before its objects. Runs are
  serialised and never cancelled mid-upload.
- **Secrets.** `R2_ACCOUNT_ID`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` and
  `R2_BUCKET`. The upload step is skipped unless all four are set, so the
  workflow publishes nothing until they are added. The key needs write access to
  that one bucket and nothing else.
- **Not done:** the build re-encodes every object on each publish, which is fine
  while packs are small; later it can fetch the live `objects/` listing first.
