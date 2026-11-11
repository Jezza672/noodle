# Brand assets

Everything here is generated, so change the generator, not the files:

```sh
node assets/brand/tools/build.mjs
```

The rules for using these files are in [docs/brand.md](../../docs/brand.md).

| Folder | What |
|---|---|
| `logo/` | The Medium wordmark (`wordmark-*.svg`), the Bold variant (`wordmark-bold-*.svg`) and the app icon tile (`icon.svg`). Colourways: `lime`, `ink`, `white`, `limeink`. Letters are outlines, so nothing needs the font installed. The wordmark's ports are real holes, so it sits on any background. |
| `textures/` | Ten repeating patterns in six themes (`canvas`, `lime`, `paper`, `blue`, `coral`, `quiet`), 1920 × 1080, named `<pattern>-<theme>.svg`. They are experiments, not part of the brand rules. `quiet` is the tone-on-tone set meant for real backgrounds. |
| `tools/` | `build.mjs` makes the files. `outline.py` turns the letters into outlines (`oodle.json`), from the Space Grotesk fonts in `crates/noodle-app/assets/fonts` and the Bold weight from the same family. Fonts are SIL OFL. |

Patterns: `diagonal-wordmark`, `n-tessellation`, `n-tessellation-diagonal`, `waves`, `waves-60`, `waves-90`, `waves-120`, `n-lattice`, `n-lattice-level`, `tile-confetti`.

Not done yet: PNG exports at 16 to 1024 px, `.icns` and `.ico` for the app bundles, and a social card.
