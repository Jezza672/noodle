# Sample packs

The design is ADR 0012; open work is in [../ROADMAP.md](../ROADMAP.md).

## Chunk 4: noodle-publish (#511)

`noodle-publish` builds the static site a source serves (FLAC objects named by
the hash of their decoded audio, a manifest per pack, `index.json`), checking
each object by decoding it again. `noodle packs validate | build | verify` drive
it, and `.github/workflows/packs.yml` validates on PRs and, on merge to main,
builds and uploads to R2 when the four `R2_*` secrets are set. Details in
[../architecture/packs.md](../architecture/packs.md). Not yet run against the real bucket.
