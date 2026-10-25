# M6: Performance and robustness

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** M6 is done in code. Batches 1 and 2: plugin delay compensation,
per-node CPU meters, parallel graph processing on worker threads, and
profiling tools (`noodle profile`, block statistics). Batch 3: out-of-process
plugin hosting (off by default; View > Plugins or `--sandbox-plugins`) and
real-time priority for the worker threads. Owed: a hand check of both on the
Mac and Windows (plugins in host processes with a real plugin; `noodle profile
--threads` showing real-time workers), and the macOS audio workgroup join.
A plugin that changes its latency after loading (restart request, latency
`changed`) is not followed until the node is rebuilt.

- Running the graph in parallel across worker threads (done: step dependency
  graph, buffers that keep branches apart, an unparking worker pool, workers
  at real-time priority where the system allows).
- Plugin delay compensation (done: CLAP and VST3 latency, delay lines in the
  plan, offline renders trimmed to the timeline).
- Out-of-process plugin hosting (done: a host process per plugin node,
  audio through shared memory, silence and a restart when it dies or hangs;
  editors of sandboxed plugins embed in host-made windows on macOS and
  Windows as in the app, with the host's main thread as its plugin thread;
  only checked by CI compiling them and by the Linux emulation of that
  arrangement, so the Mac and Windows hand check is owed, including that the
  editor really embeds and gets focus from a background host).
- CPU meters per node (done: View > CPU meters), and profiling tools (done:
  `noodle profile`, per-block statistics with deadline misses).
