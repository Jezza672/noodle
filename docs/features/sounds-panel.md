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

**Owed.** The built-in address is a placeholder until the first publish.

## Chunk 6: Sources settings (#513)

A Sources tab in Settings, reached from the panel's "Sources…" button, with
story S32 and its route.

- **Self-hosted sources:** an address and an optional token or user and
  password. The address is tidied (`index.json` and a trailing slash dropped),
  a login is refused over plain http except to this computer, and a login in
  the address is refused. The page and the add form say Noodle can't vouch for
  such a source. The secret goes in the system keychain on macOS and Windows
  (`keyring`, `sounds/vault.rs`) and in the preferences file elsewhere, which
  the page says; sources and secrets are never exported or saved in a project.
- **Folders** (`sounds/folder.rs`): scanned recursively (no links, no hidden
  files), each audio file hashed by its decoded audio, named by its file, tagged
  by its folders, licence `Unknown` (new `Licence::Unknown`, refused in a
  published pack) until set on the page. Read in place: a sound is checked
  against its hash and played from where it is, nothing is copied or uploaded.
  Rescanned on opening and on Refresh, reusing hashes of files whose size and
  time are unchanged.
- **Storage:** each source shows its sounds, how many are downloaded and the
  space they take. Clean up removes downloads no source lists (after a
  confirm). Removing a source forgets its login and leaves downloads for
  Clean up.
- The panel's source filter is now a row of chips, with the add-a-folder
  command on the right-click menu of "Sources…". Everything works offline from
  the saved catalogues.
- Tests: a local HTTP server that wants a token, folder scans, the cache.

**Owed.** No file browser exists in Noodle, so there is no right-click on a
folder there; "Add folder…" is on the Sources page. The macOS and Windows
keychain code is untested here (CI builds it) and needs a hand check on the
Mac.

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

## Chunk 7: Make shareable pack (#514)

- A wizard (`sounds/wizard.rs`), from File ▸ Make Shareable Pack… or the
  right-click menu of a folder source (or of "Sources…") in the Sounds panel.
  It reads and hashes a folder on a worker, then sets name, id, author, licence
  (default, an SPDX id or the author's own text), tags and credit for the pack
  or per sound, with bulk apply (author, tag, licence) over the included sounds.
- The default licence is "use in your own projects, commercial or not, not
  redistribute".
- The result panel is `noodle_packs::validate_dir`, the check CI and the CLI
  make, so a pack that passes here passes there. Export is disabled until it
  does.
- Export writes a static site (`noodle_publish::build_packs`) to a folder, or
  `<id>.zip` (`build_zip`). It never uploads, and refuses a folder inside the
  one being packed. A folder export can be added as a source from the wizard: a
  `file://` source read from disk.
- The pack's own definition (the RON for a PR to the built-in source) is still
  made by the CLI; contributor upload credentials remain open.
- Story S33 and its route; S09's "Place on…" step now waits for the submenu
  to open instead of settling a fixed time (#530).
