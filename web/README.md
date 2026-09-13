# kindboard website (`web/`)

Static, self-contained presentation site for kindboard.

## Deploy anywhere

- **Every asset reference is relative** — no leading slashes, no domain
  assumptions, no CDN, no build step. The site works identically:
  - opened locally via `file://` (double-click `index.html`),
  - served from any static host,
  - hosted under any subfolder of any domain (e.g.
    `https://example.com/projects/kindboard/`).
- Upload the contents of this folder (index.html, styles.css, app.js,
  favicon.svg, assets/) to any web root — nothing else is needed.

## Before publishing

1. Replace the **two flagged GitHub placeholder links** (header "View on
   GitHub" button and the footer "GitHub" link — both marked with an HTML
   comment) with your repository URL.
2. Update the sample clone URL in the Getting Started code block
   (`https://github.com/<owner>/kindboard.git`, also flagged).
3. Optionally replace the screenshots in `assets/img/` with your own
   captures — keep the same filenames (the page references them).

## Files

| Path | Purpose |
|---|---|
| `index.html` | Single-page site (hero, features, architecture, screenshots, themes, CNI matrix, getting started, roadmap) |
| `styles.css` | Brand-matched design system (teal-blue accent, dark hero, light content sections) |
| `app.js` | Vanilla JS: mobile nav, scroll-spy, image lightbox |
| `favicon.svg` / `favicon-192.png` / `apple-touch-icon.png` | O.R.P mark in browser-tab and touch sizes |
| `assets/img/orp-mark.svg` | O.R.P mark only (transparent — used in the header next to the HTML wordmark) |
| `assets/img/logo-orp.svg` | O.R.P full lockup (transparent background, light text for dark surfaces — used in the footer) |
| `assets/img/screenshot-*.png` | Real app captures: overview ×3 themes, cluster ×2 themes, wizard |
| `og-image.png` | Social-share image (O.R.P mark) |
