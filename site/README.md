# Website (what noodledaw.com serves now)

The "coming soon" holding page: one static file, `index.html`, with no build step, no scripts and no links. Open it in a browser to preview it.

- The background is the wordmark repeated on a diagonal, drawn as an SVG pattern from the outlined wordmark. It follows the system light or dark setting.
- Fonts (Space Grotesk and Inter) load from Google Fonts.
- `favicon.svg` is the app icon tile (`assets/brand/logo/icon.svg`).

The full landing page lives in [`site-next/`](../site-next/README.md). It is not served from here; GitHub Pages hosts it at dev.noodledaw.com. This page is hosted separately (a small Cloudflare Worker serving this file, deployed by hand for now).
