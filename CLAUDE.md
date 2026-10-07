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
- **Linux dependency:** once cpal is added (next step), Linux builds need
  `libasound2-dev`. Add an apt step to `.github/workflows/ci.yml`, and install
  it in a cloud session.

## Crates

| Crate | What it is |
|---|---|
| `noodle-core` | Project document: graph, commands with undo, RON files. No DSP. |
| `noodle-engine` | Node API, compiler, plans, `Controller`/`Processor`, offline `render`. |
| `noodle-nodes` | Built-in nodes. Integration tests: `realtime.rs`, `golden.rs`. |
| `noodle-io` | WAV files now; devices (cpal), MIDI and streaming later. |
| `noodle-cli` | The `noodle` command: `render`, and `play` next. |
| `noodle-app` | The egui app (M1). Just a placeholder so far. |

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
  - The all-platform run is triggered **by hand**
    (`gh workflow run CI --ref main`), and the user wants it **once all of
    M1 is done**. They wrote "M1" while M0 was in progress. If that matters,
    ask whether they meant M0.
  - macOS used to be tested locally on the user's Mac. A cloud session can't
    do that, so if a change could behave differently on macOS (device code,
    for example), ask the user to run the tests locally.
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

1. **Start here: finish M0 with live audio:** cpal output in `noodle-io` driving
   `Processor::process`, and a `noodle play <project>` command (play until
   Ctrl-C). Keep the device code thin, because cloud sessions have no audio
   device. Test everything up to the device boundary, and ask the user to try
   `noodle play` on their Mac. Add `libasound2-dev` to CI. Then mark M0 done
   in ROADMAP.md.
2. **Start M1:** the egui app shell and node editor. See the roadmap.
