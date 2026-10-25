# Performance

## Delay compensation

A node reports how late its outputs sound with `Node::latency` (samples; a
plugin's reported latency, or a lookahead's). It is read once, when the plan is
built, right after the node is created, and clamped to `MAX_LATENCY` (two
seconds at 96 kHz). A node that carries over from the previous plan keeps the
latency recorded in `PlanInfo`.

`plan::compensate` runs after the plan's nodes are made. Walking the schedule,
it gives every signal buffer an *arrival*: the slowest of its writer's inputs
plus the writer's own latency. Where paths meet, the early ones get a
`DelayLine` so everything lines up with the slowest:

- a node with several signal inputs lines them up with each other, and
  spectral inputs join in: a spectral input that arrives early gets a
  `SpectralDelay` (a ring of whole frames, each stored with the time it is due,
  so any delay works and not only multiples of the hop), writing a private
  spectral buffer appended to the plan's like the signal lines' regions;
- every *sink* (an Output node that plays, or a tap) lines up with the
  slowest sink, so two Output nodes on different devices stay in sync;
- the lines read their source at the consumer's step and write a private region
  appended to the plan's pool, so the compiler's buffer reuse is not touched,
  and cost nothing when no node reports latency;
- a feedback loop's own input is never compensated (the loop already carries
  its delay), and event ports are not compensated at all (a plugin's latency is
  in its audio, and notes arrive on time);
- a plan swap hands each line's contents to its successor (keyed by node,
  input and delay), and a transport jump clears them with the nodes.

A plan swap is seamless when its delay lines match by node, input and delay,
whatever their slots, so unrelated edits don't fade the output. The mix as a
whole is then `output_latency` samples late (`Controller::output_latency`).
Live playback just sounds that much late, as in any DAW, and recording
subtracts it along with the device's output latency (`Session::stop_recording`, MIDI takes); offline renders (`render`, `render_project_streaming*`,
`render_taps`, so exports and freezes) render that many extra frames and drop
them from the front, so what they write lines up with the timeline.
`noodle-nodes/tests/pdc.rs` covers the alignment and the trim,
`realtime.rs` that it never allocates through plan swaps and seeks, and
`noodle-plugins/tests/latency.rs` that CLAP and VST3 plugins report theirs
(the test plugins read `NOODLE_TEST_PLUGIN_LATENCY`).

## CPU meters

When a `Telemetry` hub is set, each node's step is timed
(`Instant::now` around its `process`, no allocation) and reported through
`Telemetry::open_cpu`/`read_cpu`/`read_all_cpu` as a share of the block's
time (`CpuReading`: a smoothed `load` and a slowly falling `peak`; 1.0 is the
whole block). Without a hub nothing is timed, so offline renders pay nothing.
The editor's View > CPU meters shows each node's load in its header, coloured
amber from 15 % and red from 50 % (or a peak over 90 %), and the graph's total
in the corner. A group has no step of its own (it is flattened away), so its
header shows the sum of the nodes inside it at any depth. A split node (feedback loop) reports its output half; the input
half's time is not counted. Time spent in delay lines is not attributed to a
node.

## Parallel processing

`engine_with_workers(settings, n, always)` starts `n` helper threads beside
the audio thread (the processor's half, `Processor::start_workers`, is
crate-private so the controller always compiles to match). Off by
default (the app's audio settings, `noodle play --threads` and
`noodle profile --threads` turn it on). The
single-threaded path is unchanged, and a graph that can't use helpers runs
exactly as before.

**What can run at once.** A plan's steps are in a valid order for one thread,
and `plan::step_graph` turns that order into a dependency graph
(`parallel::StepGraph`) from the buffers each step reads and writes: a step
waits for the writer of every buffer it reads (and every event buffer), and,
because the compiler reuses buffers, for the readers and writer of the buffer
it overwrites. A split (feedback) node's input half waits for its output half,
which holds the instance. Delay compensation lines are part of the step that
owns them. The graph is built off the audio thread, with the plan, and
`PlanShape` (`Controller::plan_shape`) reports its steps, longest chain and
width.

**Buffers.** Reusing a buffer is what stops two branches overlapping: the
second branch would take the first's freed buffer and have to wait for it.
When workers are on, the compiler (`Options::independent_buffers`) hands a
freed buffer only to a step that already follows everything that used it in the
signal flow (bit sets of each step's ancestors), so reuse adds no ordering. A
plain chain still uses two buffers; side-by-side branches each keep their
own. The cost is memory for wide graphs.

**Background threads don't poll.** Everything off the audio thread that feeds
playback (disk streams, cached-render playback, the track inputs' hub and
stretch workers) is a task on one of two shared pools in `noodle-io`
(`pool::io`, for tasks that never wait on another task, and `pool::control`,
for the ones that may: `2 + cores / 4` threads each, started on first use).
A task returns what it wants next (`Again`, `Idle`, `After(d)`, `Done`) and
sleeps until its `Bell` is rung. The audio thread rings it when it has made
room (a chunk returned, a seek, a lane's ring drained, the playhead moved an
eighth of the lookahead). A stopped project with open streams wakes nothing
(`tests/idle_wakeups.rs`), and 16 track inputs start no threads of their own
(`tests/thread_count.rs`). The extra-device `RateConverter` keeps a thread but
waits on a `Doorbell` that the main stream's callback rings.

**Running a block.** Each step claims itself with a compare-and-swap on its
count of unfinished prerequisites (a plan-sized array of atomics reset at the
start of the block, no queue), runs, and decrements its dependents. The audio
thread works on the block too and returns when every step is done and every
worker has left (the block's data is on its stack). Workers spin for about
40 µs after a block and then park; the audio thread unparks only the ones that
are parked. Nothing locks or allocates, and a panic in a node is caught, so
the block still finishes: that node's outputs are silent for the block and the
block counts in `BlockStats::panics` (the single-threaded path still
propagates the panic). Workers flush subnormals like the audio thread (the
mode is per thread; `workers_flush_subnormals`). `tests/parallel.rs` checks
the sound is bit-identical to one thread's, and counts allocations on every
thread.

**Priority.** Each worker asks the system, as it starts, to schedule it better
than an ordinary thread (`noodle_engine::raise_current_thread`, `priority.rs`; the
period is the block's duration): on macOS a time-constraint policy, on Windows
MMCSS "Pro Audio" (falling back to the highest ordinary priority), on Linux a
nice value of -10 (needs `RLIMIT_NICE`). Not `SCHED_FIFO` on Linux: cpal's ALSA
callback thread is an ordinary thread, and helpers that outrank it and spin
while they wait could make a block later than without them. A refusal is not an
error: `Processor::worker_priorities()` and `noodle profile --threads N` say
how many workers got real-time priority and how many a raised one. Joining
the output device's audio workgroup on macOS (so the scheduler places the
workers with the I/O thread) is not done: it needs the device's workgroup
handle from Core Audio, which cpal does not give us.

**Output.** An Output node mixes into its own scratch buffer; the buffers are
added to the device output in schedule order at the end of the block. The sum
differs from the single-threaded one only in the last bits (a voice sum is
grouped differently).

**When it pays.** Handing a block to other threads costs a few microseconds a
core, so unless `always` is set a plan runs on workers only if it has at least
4 steps and 1.5 times as many steps as its longest chain. That counts every
step as equal; `noodle profile --threads N` measures the real thing. Known
limits: a worker descheduled in the middle of a step stalls the block (as in any such
design); more workers than idle cores makes things worse; `mix` of several
voices is one step, so one fat node limits the speedup.

## Profiling

`noodle profile project.ron` renders as fast as it will go
(`noodle_engine::profile_project`, `noodle_nodes::profile` for projects with
clips) and prints: the render's speed against real time; per-block load
(`BlockStats`: mean, 99th percentile, worst, and how many blocks ran over
their budget, which are underruns on a live stream); each node's total time
and share, with the group it sits in; and the graph's shape (always the shape a plan for workers has, even in the
one-thread run). `--threads N` renders again with helpers, with the same
rule `play` uses for when helpers take part (`--always` forces them), and
prints the speedup.

`Telemetry::block_writer` times every `Processor::render` call (a device
callback, workers included: the real deadline, not the chunks a loop wrap or
fade cuts it into) into atomics, read with `Telemetry::read_blocks` (a 12-bucket
histogram: tenths of the budget up to 1.0, then up to 1.5, then beyond) and
cleared with `reset_blocks`; per-node totals come from `CpuWriter` through
`read_all_cpu_totals`. With CPU meters on, the editor shows the worst block of
the last few seconds under the graph total, red when any ran over.
