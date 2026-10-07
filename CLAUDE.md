# Working on Noodle

Noodle is a node-based DAW written in Rust: a Blender-compositor-style node
editor for audio, with a timeline. Read these before changing anything:

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): how it works and the rules
  each part follows.
- [docs/ROADMAP.md](docs/ROADMAP.md): milestones, and the **Status** line on
  the current one.

Settled decisions (Rust + egui, one global graph, GPLv3, polyphony as a voice
dimension, config vs parameters) are recorded in those docs. Don't reopen them
unless the user does.

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
  the levels and pitch, and say so in the PR so the user can listen.
- **Linux dependency:** cpal needs `libasound2-dev`. CI installs it; in a
  cloud session, run `apt-get install -y libasound2-dev` first.
- **No audio device in the cloud.** `noodle play` fails cleanly there. To run
  the real stream path anyway, point ALSA at a null device with
  `printf 'pcm.!default { type null }\n' > ~/.asoundrc` (and delete it after).

## Crates

| Crate | What it is |
|---|---|
| `noodle-core` | Project document: graph, commands with undo, RON files. No DSP. |
| `noodle-engine` | Node API, compiler, plans, `Controller`/`Processor`, offline `render`. |
| `noodle-nodes` | Built-in nodes. Integration tests: `realtime.rs`, `golden.rs`. |
| `noodle-io` | WAV files, and output through cpal (`DeviceWriter`, `play`). MIDI and streaming later. |
| `noodle-cli` | The `noodle` command: `render`, and `play` (reloads the file when it changes). |
| `noodle-app` | The egui app (M1). `session.rs` owns the project, undo, files and audio; views return `Edit`s. Tests use `egui_kittest`. |

## Rules the code relies on

- **Real-time safety.** Nothing reachable from `Processor::process` may
  allocate, free, lock or do I/O. `noodle-nodes/tests/realtime.rs` enforces
  this with a counting allocator. Extend it when you add audio-thread code.
- **Every graph change goes through a `Command`.** The graph's mutators are
  crate-private, so undo always works. Get new node IDs with
  `Project::new_node_id`.
- **Config or parameter.** A setting is config only if it changes a node's
  ports or shapes. Everything else is a parameter, so it can be modulated.
- **Port indices.** Nodes refer to ports by position
  (`const CUTOFF: usize = 1`). A derive macro is planned for M3.
- **Output nodes are mixed at their step in the schedule**, not at the end of
  the block, because buffers are reused after their last reader.
- **Docs move with code.** If a change alters something ARCHITECTURE.md or
  ROADMAP.md describes, update them in the same PR.

## How the user wants work done

- **One branch and PR per chunk** of work, roughly one PR's worth of a
  milestone. Never commit straight to `main`.
- **Merging:** squash-merge the PR yourself
  (`gh pr merge --squash --delete-branch`) once Linux CI passes, the tests
  pass, and Copilot's review has been dealt with (see below).
- **CI minutes are metered** because the repo is private, with macOS counting
  10× and Windows 2×.
  - Pushes and PRs run **Linux only**.
  - The Windows run is triggered **by hand**
    (`gh workflow run CI --ref main`, or the GitHub MCP's workflow-run
    tool), **once each milestone's code is merged**. The user confirmed
    this for M0 on 2026-10-07. It runs Linux and Windows.
  - **macOS runs on the user's Mac, not on Actions.** The Mac is connected
    to the Claude project, so a cloud session can start a Remote Control
    session there to run fmt, clippy and the tests on a branch, and a short
    `noodle play`. Do that for each milestone, and for any change that could
    behave differently on macOS (device code, for example).
- **Copilot reviews every PR automatically,** about 2–3 minutes after it's
  opened. Wait for the review before merging.
  - Decide whether you agree with each comment.
  - **Fix the ones you agree with in the same PR**, before merging. Don't
    defer review fixes to a later PR.
  - Leave a PR comment saying what you fixed, and which comments you
    rejected and why.
- **Prove that tests can fail.** For a new checker or invariant test,
  deliberately break the code, confirm the test catches it, then restore.
  Watch for a mutation landing in code that never runs; that happened once.
- **Keep the user informed:** after each chunk, a short summary of what
  landed, what the tests caught, and what's next.
- **The user** knows Rust, C++ and TypeScript, so explain design trade-offs
  rather than basics.

## What's next

1. **M1 is in progress,** split into parallel streams, each with its own
   branch and PR: app shell, node editor, parameter widgets and properties,
   telemetry with scope and meter nodes, and audio input with a device
   picker. The roadmap's M1 status line says what has landed.
2. **When the user is back:** run the batched macOS checks on their Mac
   (tests on the latest main, and a `noodle play` listening test).
