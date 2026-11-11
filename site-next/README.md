# Website (full landing page, not live yet)

The full landing page for noodledaw.com, held back while [`site/`](../site/README.md) (the coming-soon page) is what the domain serves: one static file, `index.html`, with no build step. Open it in a browser to preview it.

- Light (paper) and dark themes. It follows the system setting, and the button in the nav overrides it and is remembered.
- The textures are drawn by the script at the bottom of the page from the brand marks (see [docs/brand.md](../docs/brand.md) and `assets/brand/`). The three pictures in the feature cards are drawn illustrations, not screenshots; replace them with real screenshots once the app's look settles.
- Fonts (Space Grotesk and Inter) load from Google Fonts. Self-host them if the site should work without that request.
- `favicon.svg` is the app icon tile (`assets/brand/logo/icon.svg`).

Hosting: GitHub Pages serves this folder at dev.noodledaw.com. [.github/workflows/pages.yml](../.github/workflows/pages.yml) deploys it on every push to `main` that touches `site-next/`, and can be run by hand from the Actions tab. `CNAME` names the domain. The DNS record is a grey-cloud (DNS only) CNAME from `dev` to `jezza672.github.io` in Cloudflare, so GitHub can issue the HTTPS certificate.

One-off repo setting: Settings > Pages > Source: GitHub Actions. GitHub Pages allows one custom domain per repo, so the coming-soon page in `site/` is not served from here.
