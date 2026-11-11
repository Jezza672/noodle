# Website

The landing page for noodledaw.com: one static file, `index.html`, with no build step. Open it in a browser to preview it.

- Light (paper) and dark themes. It follows the system setting, and the button in the nav overrides it and is remembered.
- The textures are drawn by the script at the bottom of the page from the brand marks (see [docs/brand.md](../docs/brand.md) and `assets/brand/`). The three pictures in the feature cards are drawn illustrations, not screenshots; replace them with real screenshots once the app's look settles.
- Fonts (Space Grotesk and Inter) load from Google Fonts. Self-host them if the site should work without that request.
- `favicon.svg` is the app icon tile (`assets/brand/logo/icon.svg`).

Hosting: GitHub Pages serves this folder. [.github/workflows/pages.yml](../.github/workflows/pages.yml) deploys it on every push to `main` that touches `site/`, and can be run by hand from the Actions tab.

- The domain is the one line in `CNAME`. It is `dev.noodledaw.com` while the site is in preview.
- **Going live on noodledaw.com:** change `CNAME` to `noodledaw.com`, set the same custom domain in Settings > Pages, and point DNS at GitHub in Cloudflare (a DNS-only CNAME from `www` to `jezza672.github.io`, and for the apex either a CNAME flattened by Cloudflare or A records for GitHub Pages). Keep the records grey-cloud so GitHub can issue the HTTPS certificate.
- One-off repo setting: Settings > Pages > Source: GitHub Actions.

The "coming soon" holding page is not in this repo. It runs as a separate Cloudflare Worker until release.
