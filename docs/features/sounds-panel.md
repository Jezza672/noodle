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
  timeline (a clip that names the sound by hash, with the credit in
  `AudioClip::credit`; no copy, see chunk 8 below).
- Unreachable sources are greyed and keep the catalogue saved last time.
- Tests use a fake source (`source::fake`); no test touches the network.
  The S31 route searches, auditions and drops a sound on a Sampler.

**Owed.** The Sources settings page and adding a folder or URL source (chunk
6); the built-in address is a placeholder until the first publish.

## Chunk 8: clips use the hash store (#515)

- `AudioClip::hash` names a clip's audio by content; `source` is its display
  name. `AudioClip::file(base)` finds it: `<project>/samples/<hash>.wav`, else
  the file registered for the hash (`register_audio_file`), else by path for a
  clip without a hash. Project format 5.
- A sound dropped on the timeline, or a file imported there, makes a hashed
  clip with no copy and no saved project needed. Saving writes every hash a
  clip or sampler names into `samples/` (`SampleStore::save_file`).
- The Sounds panel's `sounds/` copy step is gone. Clips from recording, stems,
  bounce and take merge still point at the files they wrote, by path; the
  engine plays both.
- ADR 0004 records where the store sits.
