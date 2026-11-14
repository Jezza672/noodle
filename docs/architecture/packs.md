# Sample packs: publishing

The design is ADR 0012; the words are in CONTEXT.md ("Sample packs"). This file
covers what is built: the pack definition, the site `noodle-publish` builds, and
the CI workflow that uploads it. The downloader and the Sounds panel are not
built yet.

## Making a pack

Only the person making a pack does this (Jeremy for now; contributors later).
The audio never goes through git:

1. Put the audio files and a `pack.ron` in a working directory, `<dir>/<pack id>/`.
   Each sample's `hash` is `Sample::hash()` of its decoded audio and `file` names
   the audio relative to the pack directory. `noodle packs validate <dir> --audio`
   checks the definitions, the files and the hashes, and prints the hash a file
   really has when one is wrong.
2. `noodle packs upload <dir>` builds the FLAC objects (below), checks each by
   decoding it again, and uploads them to the bucket. It needs write credentials in
   `R2_ACCOUNT_ID`, `R2_BUCKET`, `R2_ACCESS_KEY_ID` and `R2_SECRET_ACCESS_KEY`.
3. Raise a PR adding `packs/<pack id>/pack.ron`. The `file` fields are
   dropped or left, since CI never reads them. CI runs `noodle packs validate packs`.
4. On merge, CI runs `noodle packs publish packs`: for every sample it fetches
   the object, checks it decodes to the hash in its name (so a missing or wrong
   upload fails the publish and writes nothing), then writes each manifest with
   `bytes` taken from the stored object, and `index.json` last.

CI holds the only credentials that write manifests and the index. For now the
person uploading uses their own R2 key; when contributors upload, they will need
a key of their own, and a bucket-wide write key can overwrite objects by means
other than the CLI, which only ever adds. That is the open question to settle
before then (a signing service, or a contributor key reviewed and rotated).

## The site

```text
index.json                      noodle_packs::Index, packs sorted by id
packs/<pack id>/manifest.json   noodle_packs::Manifest, author and licence resolved per sample, `bytes` filled in
objects/<hash>.flac             one immutable object per sample
```

`noodle-publish` builds it (`build`), and `upload` and `publish` write it to a bucket in two steps. An object is 24-bit FLAC of the sample as
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
noodle packs validate <dir> [--audio]    # definitions; with --audio also files and hashes
noodle packs build <dir> --out site      # the site above (also how to self-host)
noodle packs verify site                 # every object decodes to the hash in its name
noodle packs upload <dir> [--to dir]     # build, verify, add the objects to the bucket
noodle packs publish <dir> [--to dir]    # check the objects, write manifests and index.json
```

Tests: `crates/noodle-publish/tests/publish.rs` builds from generated WAVs, then
checks the objects decode to the same audio, a rebuild writes nothing, wrong
hashes, lossy audio, missing files and duplicate ids are refused, and `verify`
catches a flipped byte and a missing object.

## CI: `.github/workflows/packs.yml`

- **Pull request** touching `packs/**` or the two crates: `packs validate packs`
  (definitions only).
- **Push to main** touching the same paths: validate, then `packs publish packs`
  (R2 through the S3 API: `noodle-network`'s `s3::Bucket`, Signature V4 over `ureq`,
  no AWS CLI). Runs are serialised and never cancelled part way through.
- **Immutable.** Object puts are conditional (`If-None-Match: *`), so an existing
  name is never replaced, and `publish` writes only manifests and the index, which
  do change. Manifests go up after every object of every pack has been checked.
- **Dry run.** `--to <dir>` on `upload` and `publish` uses a directory as the
  bucket; the tests do this (`DirStore`).
- **Secrets.** `R2_ACCOUNT_ID`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` and
  `R2_BUCKET`. The publish step is skipped unless all four are set, so the
  workflow publishes nothing until they are added.
