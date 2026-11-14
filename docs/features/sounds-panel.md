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

**Owed.** The built-in address is a placeholder until the first publish; dragging
onto the timeline needs a saved project.

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
