# Caching

Export, freeze and offline-only nodes all rely on one property:
**`Engine::process` doesn't care who calls it.** The offline renderer runs a
plan faster than real time on a worker thread and writes the output somewhere
other than a device.

**Node modes.** Each node type declares one of two modes:

- **Realtime:** processes one block at a time.
- **Offline:** needs its whole input before it can produce output (reverse,
  normalize, high-quality time-stretch, whole-file spectral processing).
  Offline nodes implement a render-over-range API, and their output is always
  a cached render.

**What counts as cacheable.** A node's output is cacheable if it's a pure
function of the timeline: everything upstream is deterministic and nothing
depends on live input or a device. A node type says it isn't with
`Layout::nondeterministic()` (the audio input, MIDI In). A node on a feedback
loop isn't cacheable either, and neither is anything that follows a node that
isn't. An offline node with a non-cacheable input gets a `NotCacheable`
diagnostic, and plays silence.

**Cache keys.** Keys are computed Merkle-style, by `noodle_engine::analyze`.
It compiles the project as the live engine does, with groups flattened and
automation lanes turned into nodes, and walks the schedule upstream first:

```
key(node) = hash(node id, type_id, type_version, config, sample_rate, range,
                 tempo map, each input: constant | key(source output), shapes,
                 NodeType::cache_inputs(…))
key(output) = hash(key(node), output index)
```

- The node's ID is in the key because its random seed comes from it, so two
  identical noise nodes render differently.
- The lane that drives a parameter is a node in the compiled graph, so its
  points are in the key of everything it reaches.
- The timeline lives outside the graph, so a node type contributes it through
  `cache_inputs`. A track input hashes each clip it plays: its position, its
  gain and fades, and for audio the *content hash* of the file
  (`noodle_io::FileHasher`, remembered while size and modification time hold),
  so a file replaced under the same name gives a new key.
- Every key also holds the crate version and `DSP_GENERATION`
  (`noodle-engine/src/cache.rs`), so a build whose sound changed never replays
  renders from the shared per-user cache. Bump the constant with any
  regenerated golden render.
- The render length is in every key, so lengthening the project (a clip
  dragged further right) renders every frozen target again, even those it
  doesn't feed. The length reaches the last clip, automation point or tempo
  change, plus a 4 s tail, and at least 30 s. Past it a frozen node is
  silent, so a tail longer than that, or a drone that should play on, is cut.
- Clip files are hashed whole, but never on the UI thread. The UI keys the
  project with `Freezer::analyze_known`, which only looks at a file's size and
  modification time (`FileHasher::peek`) and lists the files it has no hash
  for. `Freezing` hashes those on a thread of its own (`spawn_hash_files`);
  until it finishes, no render starts, offline nodes are silent, frozen nodes
  play live, and their badges say *waiting*. The session compiles again when
  the hashes are in. The renders themselves (`freeze`, `export`) hash on their
  own threads with the blocking `analyze`. A file that can't be read is
  remembered as such until its size or time change, so it isn't retried every
  compile.
- **Watching clip files.** Once a second the session looks at the size and
  modification time of every audio file a clip plays (`filewatch.rs`; a
  `stat` each, no platform watcher). A changed file schedules the clips
  again, which opens new streams (a clip's source includes the file's
  modification time, so streams on the old file are dropped), and compiles
  again, which keys frozen and offline nodes afresh and renders them. Waveforms
  watch their files separately (`timeline::sources`). A file rewritten in
  place with its modification time put back is not seen, the same trade-off
  the hasher makes.
- An offline node can take a modulated parameter: the render taps the wire
  like any other input, and a wire into an offsetting parameter is turned into
  the parameter's value by `apply_offset`, exactly as the real-time plan does
  (`value = from_travel(to_travel(base) + signal)`). The node then reads that
  parameter as a signal over the whole range, like any input (`io.inputs[P]
  .lane(0, 0)` is the value at each frame), so a ratio or a target level can
  move along the range. It can't take events yet; that is reported as not
  cacheable.
- Renders are stored on disk by key. An edit produces a new key, so stale
  data can never be served, and undoing an edit brings the old render back
  straight away. Bump a node type's `version` whenever its DSP changes.

**Targets.** What gets rendered is a *target*: an offline node, or a node or
group the project marks frozen (`Project::frozen`, set with
`Command::SetFrozen`, so it undoes and is saved). A target is a set of outputs
in the flattened graph; for a group they are the nodes whose outputs feed the
group's output ports. `Freezer::plan` says whether each has its render in the
store (`Ready`), lacks it (`Missing`), or can't be rendered (`Blocked`).

**Playing from the cache.** The compiler takes `Replacements`: for an output,
a source node to play in its place. `replace.rs` rewires the readers of that
output to the source and drops every node that only fed it, so a frozen
subgraph costs nothing, track inputs and lanes included. `CachedPlayer`
(`noodle-nodes`) is the source. Live it streams chunks from the store through
a worker thread, in the manner of clip streams: the audio side takes full
chunks and returns spent ones through two lock-free queues, a jump asks the
worker to restart, and a loop wraps without a gap because the worker follows
the transport's loop range. In an offline render it reads the file itself.
Nothing in `process` allocates (`tests/realtime.rs` checks, with seeks and a
wrapping loop).

Until a render exists a frozen target keeps playing live, and an offline node
plays silence (a `CachedPlayer::silent`). An edit that changes a target's key
takes its render off at once (the app compiles again straight away), so stale
audio is never heard during a drag; only starting the new render waits for the
editing to pause. When a render finishes during playback the player is swapped
in with the playhead somewhere past its start. `Setup::position` carries the
transport's position as of the compile, and the player starts its worker at
the chunk holding it and reads the next few chunks before the plan is sent
(instantiation is off the audio thread), so the first block after the swap
plays instead of waiting for the worker.

**Rendering the cache.** `freeze` / `spawn_freeze` (`noodle-nodes`) renders
every `Missing` target, upstream first, on a background thread. A target is
rendered by `render_taps` (`noodle-engine`): the graph is cut down to what
feeds the target's outputs, a *tap* sink is wired to each output, and the
ordinary engine runs, handing each block to the tap, which writes the cache.
Renders earlier in the chain stand in as blocking players, so two frozen or
offline nodes in a row cost each only its own work. An offline node is
rendered by tapping its inputs over the whole range, running the node
(`render_offline_node`, which builds it as the compiler would, with the same
seed) and storing its outputs. A render that couldn't read a clip file is
never stored. The app keeps renders in step with the project
(`noodle-app/src/freezing.rs`): it looks at the cache on each compile, starts
one background render for what is missing, cancels it if the project changes
meanwhile, and shows a progress bar on the node.

**Batch 1, the pieces underneath.**

- `render_project_streaming` (`noodle-engine`) renders in chunks into a sink,
  with progress and cancel, so memory stays flat. `render_project` is still
  the in-memory version; both give the same samples.
- `Job` (`noodle-engine`) runs work on a thread with `fraction()`,
  `cancel()` and a non-blocking `poll()`. Dropping it cancels. Nothing in
  it touches the audio thread.
- `spawn_render` and `spawn_render_to_cache` (`noodle-nodes`,
  `background.rs`) are the offline renderer as a background service. The
  render has its own engine and a blocking track-input registry, so it
  never disturbs live playback and renders the same samples every time.
  A render with a missing or unreadable clip is *incomplete* and is never
  cached, since the cache would keep serving it after the file came back.
- `CacheKey` and `KeyBuilder` (`noodle-core`) are the Merkle hashing
  (blake3). Fields are type-tagged and length-prefixed. Deciding *what*
  goes into a node's key is `analyze`'s job, above.
- `CacheStore` (`noodle-io`, `cache.rs`) keeps one file per key: a 32-byte
  header and raw `f32`, so `read_frames(start, ..)` is a seek. That is the
  random access Reverse needs. Entries are written to a `.partial` file and
  renamed on commit, a truncated or corrupt entry is deleted and counts as
  a miss, and `evict_to` drops least-recently-used entries.

**Offline nodes.** `Reverse`, `Normalize` and `Time Stretch` (`noodle.offline.*`)
get the whole range as one signal per input and write the whole range of each
output, so every output is as long as the range.

- *Normalize* measures the peak (or the RMS) over every lane at once, so
  stereo balance is kept, and scales to `level` (dB; a wire makes the gain
  per sample). Silence passes through.
- *Time Stretch* is waveform-similarity overlap-add (WSOLA): grains of `grain`
  seconds are taken from the input at the speed `ratio` asks (2 is half speed,
  a wire moves it along the range), each shifted within a quarter window to
  line up with the one before, summed with a Hann window and divided by the
  window sum. All channels of a voice share the grain positions, so the image
  holds. The output is as long as the range: slowed down, the input's end
  falls off it; sped up, it ends in silence.

**Export** (`noodle-nodes/src/export.rs`, `spawn_export`) writes the project
to WAV (32-bit float, 24 or 16) or FLAC (24 or 16; `noodle_io::ExportWriter`,
which writes a `.partial` file and renames it, so a failed or cancelled export
leaves nothing). It hears what playback hears: the missing renders of offline
and frozen nodes are made first (the same `freeze`, into the same cache, with
the same length that playback keys them by, so they are the same renders),
then `render_project_streaming_replacing` renders the project from the start
with those renders standing in, blocking players for exact output. A range
(`start..end` in frames) only chooses which part of that render is written;
the render always runs from the start, so delays and envelopes are in the
state they would be in on the way there. Offline renders cover the playback
length (the timeline and a tail, at least 30 s); a range that ends later gets
renders of its own, as long as the range (a different key), so it never goes
silent partway through.
Offline nodes that couldn't be rendered are silent in the file and listed in
the report. Progress is the renders of nodes first (half the bar), then the
file. The app's File > Export Audio… dialog (`export_dialog.rs`) asks for a
format and the whole project (to the last clip, plus 4 s of tail) or a range in
seconds, then for a file, and runs it on the session (`session/export.rs`) with
a progress bar and cancel. Exports are stereo, at the stream's sample rate.
Integer formats cut off samples past full scale. FLAC blocks are chosen so the
last one is never shorter than FLAC's 16-frame minimum, and its header says
every block but the last is the stream's block size, as some decoders
(Symphonia) insist. FLAC frames go to disk as they are encoded, after a
placeholder header that `finish` overwrites with the totals, so memory doesn't
grow with the length. Without a modification time (some filesystems), a file's
size alone says whether its hash is still good.

Plugins are treated as deterministic by default. Each plugin node can opt
out, for plugins that use randomness.
