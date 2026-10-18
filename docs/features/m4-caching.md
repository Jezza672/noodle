# M4: Caching

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** done. Batch 1: streaming offline renders, the offline renderer
as a background job with progress and cancel, and the on-disk cache store.
Batch 2: Merkle key derivation and cacheability analysis from the compiled
graph, offline nodes running from the cache (Reverse; the compiler no longer
refuses them), and freeze for nodes and groups, with a progress bar on the
node and a cached-audio player in the plan. Batch 3: the Normalize and Time
Stretch offline nodes, offline nodes taking modulated parameters, the export
dialog (WAV and FLAC, the whole project or a range, with offline and frozen
nodes rendered first), the playhead handed to a cached player when it is
swapped in (no dropout), clip files hashed off the UI thread, and clip files
watched for changes. Known limits: an offline node still gets its whole range
in memory, a frozen node plays for a fixed length (the clips' end plus a
tail, at least 30 s), a cached render is keyed by the sample rate it was made
at, and offline nodes take no events.

- The offline renderer as a background service, with progress reporting to the UI.
- **Streaming offline renders.** Offline nodes currently get the whole range in
  memory, about 230 MB for 10 minutes of stereo. Render in chunks instead,
  with random access for nodes like Reverse that need it.
- Content-addressed cache store on disk, with Merkle-style keys.
- Analysing which nodes are cacheable.
- Freezing nodes and groups, with a progress bar on the node.
- **Offline-only node API:** the first nodes are reverse, normalize and
  time-stretch.
- Export dialog for WAV and FLAC, covering the whole project or a range.

**Done when:** you can freeze a heavy group and watch CPU use drop, an
offline node re-renders automatically when something upstream changes, and
export produces output identical to live playback.
