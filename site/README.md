# Website

The landing page for noodledaw.com: one static file, `index.html`, with no build step. Open it in a browser to preview it.

- Light (paper) and dark themes. It follows the system setting, and the button in the nav overrides it and is remembered.
- The textures are drawn by the script at the bottom of the page from the brand marks (see [docs/brand.md](../docs/brand.md) and `assets/brand/`). The three pictures in the feature cards are drawn illustrations, not screenshots; replace them with real screenshots once the app's look settles.
- Fonts (Space Grotesk and Inter) load from Google Fonts. Self-host them if the site should work without that request.
- `favicon.svg` is the app icon tile (`assets/brand/logo/icon.svg`).

Not set up yet: hosting. Serving `site/` from GitHub Pages (or any static host) is a repository setting plus a DNS record for noodledaw.com.
