# Sample packs: the Sounds panel

Chunk 5 of the sample packs plan (ADR 0012, issue #512).

**What landed.** A Sounds panel that browses sources, auditions a sound, and
drags it into the project, with a usability story (S31) and route.

- `noodle-app/src/sounds`: `catalogue` (the item model, search, filters,
  credit lines), `source` (reading a source's `index.json` and manifests,
  downloading an object with a decode-and-hash check, over `noodle-network`),
  `apply` (what a dropped sound does to the project) and the panel itself.
- Opened from View, File > Browse Sounds…, the palette and the Sampler's
  "Browse sounds…" button.
- Click to audition through a preview player outside the graph; a sound over
  8 MB asks before downloading.
- Drag onto a Sampler or pad (the hash), the canvas (a Sample node) or the
  timeline (copied into the project folder, a path-referenced clip with the
  credit in the new `AudioClip::credit`).
- Unreachable sources are greyed and keep the catalogue saved last time.
- Tests use a fake source (`source::fake`); no test touches the network.
  The S31 route searches, auditions and drops a sound on a Sampler.

**Owed.** The Sources settings page and adding a folder or URL source (chunk
6); the built-in address is a placeholder until the first publish; dragging
onto the timeline needs a saved project.
