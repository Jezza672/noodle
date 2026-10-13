# Samples are stored by content hash

A sampler's audio is referenced by the hash of its contents, not by a file path. The file itself is kept in a store beside the project, in the user library for saved groups and presets, and shipped as assets for system kits.

A preset, a saved group and a system drum kit all have to carry audio with them and keep working when moved to another machine. A path breaks as soon as the file moves; copying the file into each project makes the same kit a dozen unrelated files. A hash makes copies, presets and reloading identical, keeps one stored file however many samplers, presets and saved groups use it, and gives caches a key. We considered paths (simple, fragile) and per-project copies with relative paths (portable, duplicated), and chose the hash for portability without duplication. The cost is a store to manage and a rule for when unreferenced files are removed.

Audio clips keep their path relative to the project file for now, so there are two ways of referencing audio until clips move to the store too; that is deliberate, to keep this chunk small. Where the store sits relative to the project file is provisional and is settled when the library is built.
