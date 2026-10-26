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
key(node) = hash(node id, type_id, type_version, config, sample_rate,
                 max_frames, tempo map, each input: constant | key(source
                 output), shapes, NodeType::cache_inputs(…),
                 and for an offline node: the render length)
key(output) = hash(key(node), output index)
```

- The node's ID is in the key because its random seed comes from it, so two
  identical noise nodes render differently.
- The lane that drives a parameter is a node in the compiled graph, so its
  points are in the key of everything it reaches.
- The timeline lives outside the graph, so a node type contributes it through
  `cache_inputs`. A track input hashes each clip it plays: its position, its
  gain, gain points, fades and crossfade, and for audio the *content hash* of the file
  (`noodle_io::FileHasher`, remembered while size and modification time hold),
  so a file replaced under the same name gives a new key.
- Every key also holds the crate version and `DSP_GENERATION`
  (`noodle-engine/src/cache.rs`), so a build whose sound changed never replays
  renders from the shared per-user cache. Bump the constant with any
  regenerated golden render.
- **The render length is only in the keys of offline nodes** (and so of
  whatever follows one). An offline node reads its whole input range, so a
  longer render is a different one. A real-time node's output up to a frame
  doesn't depend on how long the render goes on, so a lengthened project
  doesn't change the key of a frozen real-time target. An entry is **ready**
  when it is *at least* as long as needed (`Freezer::plan`), and entries
  **grow**: a render's length is rounded up to a multiple of 15 s, so a project
  that grows a little is still served by the entry it has, and one that grows
  past its end is rendered again, longer, under the same key. (Growing is a
  longer render from a fresh state, not a resumed one: DSP state is not
  saved.) The length reaches the last clip, automation point or tempo change,
  plus a 4 s tail, and at least 30 s. Past it a frozen node is silent.
- **`max_frames`** (the largest block) is in every key: it is part of what
  the nodes were built with. Block-size-dependent behaviour (control-rate
  sampling of a parameter at the start of a block, say) would otherwise
  serve a render made at another size.
- **What the track input adds.** For an audio clip: its file's content hash,
  window, gain, fades, gain points and crossfade, the `extent` of a section
  dragged out past its window, and, when the clip is warped, the warp's mode,
  tempo, markers and `loop_region`, plus whether the warp preview is on
  (while it is, warped clips play as Repitch). For a MIDI clip: its length
  and notes, and `loop_on`, `loop_start` and `loop_len`.
- **Plugin latency** is not keyed separately. It is read from an instance,
  which analysis must not build, and it is a function of what is keyed already:
  the plugin file's size and time, its saved state (in the config), the sample
  rate and `max_frames`.
- **Live branches.** `Layout::deterministic` is per node *type*, so it cannot
  say that one track is being monitored right now without turning off the
  cache of every track. `CacheEnv::live` carries that session state into the
  analysis instead: a function from node to "the session is feeding this node
  live" (`ClipFeeds::is_live`: monitoring is on, or a piano-roll key is held).
  Such a node gets `Uncacheable::Live`, and everything downstream follows it
  as it does a live input. Tracks that aren't played keep their cache. Only
  `analyze_known`, the playback path, passes it; a render or export runs from
  the project alone.
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
- **Watching clip files.** One service answers "has this audio file changed?"
  for everyone: `noodle_io::AudioFiles` (`files.rs`). A `FileStamp` (size and
  modification time) is the only rule. One thread looks at the files that have
  been asked about once a second (it sleeps when there are none, and forgets a
  file nobody has asked about for 30 s) and wakes the UI (`noodle_engine::wake`)
  when a stamp differs. `AudioFiles::stamp` reads what the thread last saw, so
  the session, the clip schedule (`ClipFeeds::file_info`) and the timeline's
  waveforms (`timeline::sources`) compare stamps with no `stat` of their own,
  even during a clip drag. A changed file schedules the clips again, which opens
  new streams (a clip's source includes the file's stamp, so streams on the old
  file are dropped), and compiles again, which keys frozen and offline nodes
  afresh and renders them. A file rewritten in place with its size and
  modification time both put back is not seen, the same trade-off the hasher
  makes. The streaming hub and the warp worker still keep their own 2 s retry
  for files that fail to open.
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

**Memory.** `Layout::memory` says how long a node remembers its input:
`Memory::None` (sources, gain, mix, maths, the track input), `Bounded(ms)`
(filters, delay, reverb, EQ, dynamics; the longest decay to -60 dB the type can
make over its parameter range) or `Unbounded` (oscillators, LFOs, noise,
voices, the sampler, scripts, plugins, anything that doesn't say, which is the
safe default). `analyze` composes it into `NodeAnalysis::memory` along the
longest path: a chain sums, parallel branches take the longest, `Unbounded`
absorbs, and an offline node or a node on a feedback loop is `Unbounded`. A node
type sets it with `Layout::memoryless()` / `bounded(ms)`, or
`#[ports(memoryless)]` / `#[ports(bounded = ms)]` in the derive. The playback
cache picks its entries by it (below).

**Reset equals fresh.** The playback cache renders a pass from a fresh state at
its start (ADR 0010), so `Node::reset` must put a node back into the state a new
instance has, and a seek on a running engine must sound like an engine started
there. The state a node keeps is of two kinds. *Timeline state* is what a new
instance has at the start of a pass: filter memory, phases, random streams,
voices, a track input's sounding notes and declick gain, and a parameter's ramp
in progress (`ValueInput::settle`, which `Plan::reset_nodes` calls, ends it at
the value the parameter holds now). *Resources* are not reset, because a new
instance has the same ones to hand: a track input's streams and source windows,
a sampler's stretched copies and worker link, a plugin's last-sent parameter
values (they mirror what the plugin holds, and its own reset leaves parameters
alone). Two tests keep this honest:
`noodle-nodes/tests/reset_matches_fresh.rs` runs every registered node type
(nodes with spectral or sample inputs, and the sampler, are skipped and have
tests of their own) for 1.5 s with notes and noise, resets it, and compares
what it plays with a new instance built at the same position;
`noodle-nodes/tests/seek_matches_fresh.rs` does the same for every example
project through the whole engine, a seek against an engine told to start there.

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
a worker task on the `noodle-io` pool, in the manner of clip streams: the audio side takes full
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

**The playback cache.** On top of the freeze machinery, the app caches by
itself (ADR 0010, ROADMAP "Playback cache"). A **pass** is one run of the
transport from a start S; an entry is the audio of one cluster for one S,
rendered from a fresh state there (`render_taps` starts the engine with
`Processor::start_at`, which is a seek that needs no fade). The session
(`Session::pass_start`) begins a pass at each seek, and at the playhead when the
stream opens; pausing and playing again carries on the same pass.

- **Targets.** `TargetKind::Auto(group)` is every top-level group that is not
  frozen and can be cached whole; one that can't (a live input, a loop, a
  monitored track) is left live and doesn't hold the others back. A group
  inside a group is part of the outer cluster. The cut is *before the group's
  output stages* (`flatten::group_sources`): the gain, mute and solo of a
  track are applied to the cached signal, so a fader move or a solo changes no
  key and renders nothing, while a node inside the track changes only that
  track's key.
- **Tiers and keys.** An entry is the cluster rendered from a fresh state at
  its *origin*, keyed `pass_key(output_key, origin)`; the rendered length is not
  in it. The tier of the cluster (`Analysis::memory`: the longest of its
  outputs') decides which origins serve a pass from S (`Analysis::entries`, best
  first; the first is the one rendered when none exists):
  - *Position-pure* (`Memory::None`): origin 0 serves every start. Passes for
    0, the loop start and a seek share one entry, rendered once for the longest.
  - *Bounded(n)*: an origin at or before S−n serves S, because what the cluster
    remembers of the time before has died away. Origins are on a grid of one bar
    (3840 ticks), so the starts in a stretch share an entry; the entry from 0
    serves too. Warming is from a fresh state, so the pre-roll contains only
    audio that played.
  - *Unbounded*: origin S only.
  A cluster with no entry for the pass stays live while the others play from
  the cache. The entries upstream of a render stand in for it as for any freeze
  (`EntrySpec::pass`).
- **Playing.** `Freezer::plan(.., Some(pass))` gives the replacements for the
  entries that are ready; `CachedPlayer::starting_at(frame)` plays an entry
  from the pass's first frame (silent before it). A pass without entries plays
  live, and the render swaps in when it finishes, as for a freeze. A seek while
  entries for another start are playing recompiles at once; otherwise the cache
  is looked at when the seeking pauses (the same 250 ms debounce an edit uses),
  so dragging the playhead doesn't compile per step.
- **Rendering.** While a stream is open, `Freezing` renders the entries that
  are missing for the pass that is playing and the hot starts, 0 and the loop's
  start, in that order (`freeze_passes`; one job, a target at a time, upstream
  first, earlier entries standing in). A project with no groups, or no open
  stream, or `Session::set_auto_cache(false)`, does none of it. Export doesn't
  use the entries.
- **Loops.** A pass that starts before the loop's end wraps there
  (`Pass::looping`), and the tier decides what the wrap does, per cluster:
  - *Pure*: the entry from 0, played with the loop; nothing to carry.
  - *Bounded*: one **steady lap** of the loop, keyed by the loop. It is rendered
    by the engine itself looping (`render_taps(.., looping, ..)`), from the
    loop's start for `ceil(n / lap)` warm-up laps plus the lap that is kept, so
    a reverb or delay tail from the end of the loop is at the start of the lap,
    as in a live loop. The player loops inside it, and it serves every start
    inside the loop. The first lap of a pass therefore has the steady-state tail
    at its start, which a live first lap would not; that is the one place the
    cache is not the pass-start truth.
  - *Unbounded*: one lap rendered fresh from the loop's start, with its last 4 ms
    faded out (`LoopKind::Faded`), so oscillator phase and voices reset at the
    wrap without a click. Only a pass starting at the loop's start has it; from
    elsewhere in the loop the cluster plays live.
  - A pass that starts before the loop's start gets only the pure clusters from
    the cache.

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
  it touches the audio thread. A job calls `noodle_engine::wake` when its
  thread ends (a panic included), so the UI polls it when it finishes rather
  than on a timer.
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
