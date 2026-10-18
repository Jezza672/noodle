# M11: Spectral signals

Delivered. This is the record of what was planned and built; open work is in [ROADMAP.md](../ROADMAP.md), the design in [ARCHITECTURE.md](../ARCHITECTURE.md).

**Status:** batch 1 done in code (the signal type, Analyze and Synthesize and six
spectral nodes), and the spectrum display. The design is the docs/architecture/ files (index: ../ARCHITECTURE.md) ("Spectral") and ADR 0007.

1. **The signal type and the first nodes (done).** A third wire kind that
   carries FFT frames (one size for the whole engine, 2048 with a hop of 512),
   its plumbing through the compiler, plan, test harness, derive macro and
   editor (violet squares), `Analyze` and `Synthesize` (latency reported to
   plugin delay compensation), `Spectral Gate`, `Spectral Filter`, `Spectral
   Shift`, `Spectral Freeze`, `Spectral Blur`, `Cross`, node presets for each,
   `examples/spectral-shift.ron` + golden, and the allocation test.
2. **A spectrum display (done).** The `Spectrum` view node: an analyser curve
   with held peaks on the node and in the properties panel, with a spectrogram
   there too (the analysis runs on the UI thread, fed by the Scope's
   telemetry). Still to do in M11: more nodes (pitch shift and time stretch on frames, spectral
   compressor, convolution reverb from an impulse response, a noise-profile
   denoiser) and a choice of frame size (a config on Analyze that every
   spectral node on the wire adopts). Delay compensation across spectral wires
   is done (a `SpectralDelay` in front of an early spectral input).
