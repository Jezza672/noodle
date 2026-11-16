# Sample packs

One directory per pack: `packs/<pack id>/pack.ron`. Only the definition is in
git; the audio is uploaded to R2 first. See
[docs/architecture/packs.md](../docs/architecture/packs.md) for the flow.

A pack may also list `items`: group templates and presets, each a RON file in
the pack directory whose `hash` is the SHA-256 of the file and whose `needs` name
the pack's own samples. They are in git, unlike audio.
