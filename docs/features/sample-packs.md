# Sample packs

The design is ADR 0012; open work is in [../ROADMAP.md](../ROADMAP.md).

## Chunk 4: noodle-publish (#511)

`noodle-publish` builds the static site a source serves (FLAC objects named by
the hash of their decoded audio, a manifest per pack, `index.json`), checking
each object by decoding it again. `noodle packs validate | build | verify | upload | publish` drive
it: `upload` adds the audio to R2 before the PR, and `.github/workflows/packs.yml`
validates definitions on PRs and, on merge to main, publishes manifests and
`index.json` (after checking the objects) when the four `R2_*` secrets are set.
The S3 client lives in `noodle-network` (renamed from `noodle-download`). Details in
[../architecture/packs.md](../architecture/packs.md). Not yet run against the real bucket.

## Groups and presets in packs

Packs carry group templates and node and group presets as well as sounds
(design in [../architecture/packs.md](../architecture/packs.md)): the pack format,
`noodle packs` commands and CI publish handle `items`; the browser has a Library
tab to install them and a Share window to export your own. Entries keep their
origin (credit and licence) and a pack's groups can use the pack's own samples.
Tests: `noodle-packs/tests/packs.rs`, `noodle-publish/tests/items.rs`, and
`sounds/tests.rs` (`library_packs`).
