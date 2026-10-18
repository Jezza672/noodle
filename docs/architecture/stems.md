# Stems

Split to stems turns an audio clip into one new track per stem (drums, bass,
vocals, other) and one for the residual. The design is ADR 0006; the words are in
CONTEXT.md ("Stems"). Stem warp, the other use of stems, is not built.

- **`noodle-stems`.** `Separator` is the trait: it takes stereo audio at its own
  sample rate and returns the four stems, each as long as the input, reporting
  progress and stopping when asked. `FakeSeparator` splits by frequency band and
  leaves a residual, so tests need no weights. `OnnxSeparator` (feature `onnx`,
  on in the app) runs an HT-Demucs export through ONNX Runtime: it cuts the audio
  into 7.8 s segments that overlap by a quarter, normalises the mix as Demucs
  does, and blends the segments with triangular weights. The runtime library is
  loaded when a separator is opened (`ort`'s `load-dynamic`), from
  `libonnxruntime.*` in the model folder, `ORT_DYLIB_PATH` or the system's search,
  so the build needs no native library. `ort::init` panics on a missing library
  and, after one failed load, reports later loads as fine and panics when the
  library is first used; so the library is opened directly first, always loaded
  through `init_from`, and a failure `ort` itself gives (a too-old runtime) is
  remembered. The session loads the runtime before asking to download anything,
  so a machine without one is told so up front. The last segment ends where the
  audio does (it starts early) rather than being padded, so the model always sees
  real audio. `open` checks the model's signature (one input `[1, 2, 343980]`,
  first output `[1, 4, 2, 343980]`, so the STFT and its inverse are inside the
  graph, as in the Mixxx HT-Demucs export) and refuses any other with that
  signature spelled out; exports that keep the STFT outside (two inputs, a
  frequency and a time output) are not supported. The built-in address is Hugging Face `adowu/htdemucs-onnx` (MIT, an
  export of Demucs's MIT-licensed HT-Demucs; the Demucs README and LICENSE are MIT),
  `htdemucs.onnx`, 316 MB, pinned to one commit and to its SHA-256, whose
  signature is exactly the one above. Verified against ONNX
  Runtime 1.22.0 (the pinned download) with tiny stand-in models of that signature
  (`tests/runtime.rs`, which runs only where `NOODLE_TEST_ORT_DIR` names a folder
  with the library and the models; CI skips it), **not against real weights**.
  Memory: a split holds about ten copies of the clip's audio at once (the decode,
  the mix, four stems at the model rate and five parts at the source rate), around
  1 GB for five minutes of stereo.
- **Weights and runtime.** Neither is bundled; both are downloaded, once, after
  the app asks (a dialog that names both). `ModelStore` keeps them in the `models`
  folder of the user data directory; a download streams to a `.partial` file,
  checks its SHA-256, and renames, so a cancelled or failed download leaves
  nothing. The weights come from the pinned address above
  (`NOODLE_STEM_MODEL_URL` and `NOODLE_STEM_MODEL_SHA256` point at another export
  instead). ONNX Runtime 1.22.0 (MIT, Microsoft; well above `ort`'s minimum of 1.17, and the stand-in model tests pass against it) comes from its GitHub release
  archive for the platform (Linux x64 and arm64, macOS arm64, Windows x64; no
  Intel Mac, which Microsoft stopped building), pinned by SHA-256, and only the one
  library is kept (`RuntimeSpec`, `download_runtime`). It is kept in
  `models/onnxruntime-<version>/`, so bumping the pin downloads the new one. It is
  skipped only when that is already there or `ORT_DYLIB_PATH` is set: the system's
  own library is never probed, because `ort` keeps the first library it loads for
  the whole process (even one it refuses), which would shadow the pinned runtime
  (Windows ships an unpinned `onnxruntime.dll`). A library put in the model folder
  by hand still works. There
  is no release build of Noodle yet, so nothing is packaged with it; when there
  is, shipping the library beside the app makes the download unnecessary. The
  two downloads are the first 30% of the split's progress bar.
- **`split`.** Decodes the source up to the end of the clip's window, makes it
  stereo at the separator's rate, separates, and brings the stems back to the
  source's rate and length. The residual is the source minus the stems *as
  written*, so the five files add back up to the source. They are 32-bit float
  stereo WAVs, `<clip>-<part>.wav` in the project's `stems` folder (a number is
  added to the name if the files exist). A cancelled or failed split removes what
  it wrote. Each file covers the start of the source file through the end of the
  clip's window, so the new clips use the source's `offset` and `length` as they
  are and every position in the warp (which is in the file's seconds) stays valid.
- **In the session.** `Session::split_to_stems` needs a saved project (the stems
  go next to its file) and an audio clip. The separating, and the download before
  it, run as one `Job` with one progress bar (the status bar shows it with a
  Cancel). When it finishes, one `Command::Batch` adds, for each part, a track
  (`add_track::with_input`, so it is wired to the default mixer, built one after
  another on a copy so each takes its own mixer input) named `<clip> (<part>)`,
  a clip that is the source's section again over the part's file (start, window,
  gain, fades), and, if the source is warped, an `AddWarp` of a copy of its warp:
  never the source's `WarpId`, which would link them. It also mutes the source and
  puts the new tracks right under its track in the track order. The whole batch is
  one undo step. If the clip was removed or its window grew while the job ran,
  nothing is added and the files are deleted.
- **Clip mute.** `AudioClip::muted` (saved only when set). A muted section is not
  scheduled at all, so it costs no stream, and its cache key says only that it is
  muted. The timeline draws it faded; mute is in the clip panel and the clip's
  right-click menu, next to Split to stems. MIDI clips have no mute yet.
