# The engine runs independently of the transport

The audio stream (the Engine) opens when the app opens and stays open until a toolbar toggle turns it off; Play, Stop and rewind only move the Transport. Playing a MIDI instrument must not require starting the timeline, and DAWs like Ableton work this way. The cost is that a graph that makes sound on its own (a drone) sounds as soon as the engine is on, which we accept because silence skipping keeps an idle engine cheap.

If no real output device opens, the engine falls back to the system default and then to a virtual output that discards audio, with a warning on the toggle.
