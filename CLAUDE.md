# Working on Noodle

Noodle is a node-based DAW written in Rust: a Blender-compositor-style node
editor for audio, with a timeline. This file is an index. Read the one file
you need, not all of them.

## Where things live

| Need | Read |
|---|---|
| How it works, the rules, which file covers what | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), then one file in [docs/architecture/](docs/architecture/) |
| What is left to do | [docs/ROADMAP.md](docs/ROADMAP.md) (open work only) |
| What was built, per milestone, with PR numbers | [docs/features/](docs/features/README.md) |
| What each crate is, where its tests are | [docs/crates.md](docs/crates.md) |
| Domain terms | `CONTEXT.md`, [docs/agents/domain.md](docs/agents/domain.md) |
| Why a decision was made | [docs/adr/](docs/adr/) (read the ones near your area) |
| Issues and labels | [docs/agents/issue-tracker.md](docs/agents/issue-tracker.md), [triage-labels.md](docs/agents/triage-labels.md) |

Settled decisions (Rust + egui, one global graph, GPLv3, polyphony as a voice
dimension, config vs parameters) are in those docs. Don't reopen them unless
the user does.

## Build and test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p noodle-cli -- render examples/vibrato.ron out.wav
```

- **Toolchain:** stable, from `rust-toolchain.toml`.
- **Debug builds are optimised** (`opt-level = 1`, and 3 for dependencies),
  because unoptimised DSP is too slow to listen to.
- **Golden renders:** every `examples/*.ron` must render, within a small
  tolerance, to `crates/noodle-nodes/tests/golden/<name>.wav`. If a change
  is *meant* to alter the sound, regenerate them with
  `UPDATE_GOLDEN=1 cargo test -p noodle-nodes --test golden`, sanity-check
  the levels and pitch, and say so in the PR so the user can listen. Also
  bump `DSP_GENERATION` in `noodle-engine/src/cache.rs`, so renders cached
  by the old sound are not replayed.
- **Linux dependency:** cpal needs `libasound2-dev`. CI installs it; in a
  cloud session, run `apt-get install -y libasound2-dev` first.
- **No audio device in the cloud.** `noodle play` fails cleanly there. To run
  the engine anyway, use the virtual output: `noodle play --output virtual x.ron`,
  or `testkit::virtual_audio()` in an app test. It runs in real time and
  discards the audio; `Playback::virtual_capture()` (`Session::virtual_capture()`
  in app tests) keeps it, so a test can assert on the sound. (The app falls back
  to it by itself when no real output opens.) To run the real stream path, point
  ALSA at a null device with `printf 'pcm.!default { type null }\n' >
  ~/.asoundrc` (and delete it after).

## Rules the code relies on

- **Real-time safety.** Nothing reachable from `Processor::process` may
  allocate, free, lock or do I/O. `noodle-nodes/tests/realtime.rs` enforces
  this with a counting allocator. Extend it when you add audio-thread code.
- **Every graph change goes through a `Command`.** The graph's mutators are
  crate-private, so undo always works. Get new node IDs with
  `Project::new_node_id`.
- **Config or parameter.** A setting is config only if it changes a node's
  ports or shapes. Everything else is a parameter, so it can be modulated.
- **Port indices.** Nodes refer to ports by position. Declare ports with
  `#[derive(Ports)]` so the layout and the index constants (`SvfPorts::CUTOFF`)
  come from one place; only nodes whose ports depend on config build a
  `Layout` by hand.
- **Output nodes are mixed at their step in the schedule**, not at the end of
  the block, because buffers are reused after their last reader.
- **Docs move with code.** If a change alters something a doc under `docs/`
  describes, update it in the same PR. Finished work leaves ROADMAP.md and goes
  into the matching file in `docs/features/`.

## How the user wants work done

- **One branch and PR per chunk** of work, roughly one PR's worth of a
  milestone. Never commit straight to `main`.
- **Merging:** squash-merge the PR yourself
  (`gh pr merge --squash --delete-branch`) once CI passes, the tests
  pass, and the Opus review has been dealt with (see below).
- **CI runs on GitHub-hosted runners** (Linux, macOS and Windows) on every
  push and PR. The repo is public, so minutes are free. There is no
  self-hosted runner: never add one, because fork PRs would run on it.
  - macOS also gets a short `noodle play` listening test on the user's Mac
    (a Remote Control session) for each milestone, and for any change to
    device code. Never produce real audio output there without asking.
- **Who does what:** Sonnet agents do the work. Every PR is reviewed by a
  separate review thread running Opus at medium effort. Don't wait for
  Copilot's review; it is no longer a merge gate.
  - Before merging, start the reviewer (an Opus, medium-effort agent that
    did not write the code) on the PR diff.
  - Decide whether you agree with each finding.
  - **Fix the ones you agree with in the same PR**, before merging. Don't
    defer review fixes to a later PR.
  - Leave a PR comment saying what you fixed, and which findings you
    rejected and why.
- **Prove that tests can fail.** For a new checker or invariant test,
  deliberately break the code, confirm the test catches it, then restore.
  Watch for a mutation landing in code that never runs; that happened once.
- **Keep the user informed:** after each chunk, a short summary of what
  landed, what the tests caught, and what's next.
- **The user** knows Rust, C++ and TypeScript, so explain design trade-offs
  rather than basics.
