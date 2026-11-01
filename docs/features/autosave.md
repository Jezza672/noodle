# Autosave and crash recovery

While a project has unsaved changes, `noodle-app` writes a copy of it every
30 seconds, off the audio thread (on the UI thread, between frames; the audio
thread is never involved). If the app crashes, the next launch offers to
recover it.

- **Where.** `autosave-<pid>.ron`, `.origin` (the project file the copy came from, empty
  if untitled) and `.lock` in the data directory's `autosave` folder,
  e.g. `~/.local/share/noodle/autosave` (`autosave::default_dir`). Written
  atomically with `write_atomically`.
- **When.** `Autosave::tick`, from `App::show`, writes only when the project
  is dirty, the history position has moved since the last write and 30 s have
  passed. A clean project removes the copy, so saving clears it.
- **Crash detection.** A clean exit (`App::on_exit`) removes the copy, so one
  found at startup whose lock is free means that run did not end cleanly. Each run holds an OS lock (`File::try_lock`) on its `.lock` file, so another Noodle still running is never mistaken for a crash and its copy is left alone. `App::set_autosave`
  reads it and shows a Recover / Discard dialog. Nothing is autosaved while the
  question is open, so the copy is not overwritten. Quitting without answering
  keeps it. A copy that won't load is moved to `failed-autosave.ron`.
- **Recover.** `Session::recover` replaces the project, keeps the original
  path (so relative clip paths resolve) and marks it unsaved. Save writes
  over the original file.
- **Limits.** The copy is the project file only: not the undo history, and not
  samples held in memory that were never saved beside a project.
- **Tests.** `autosave.rs` (interval, change detection, removal) and
  `app.rs` (`a_crashed_runs_autosave_can_be_recovered`,
  `discarding_the_autosave_leaves_the_project_alone`).
