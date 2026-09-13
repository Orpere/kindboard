# kindboard website (`web/`)

Static, self-contained presentation site for kindboard.

**Live:** <https://orpere.github.io/kindboard/> — published from this folder
by the GitHub Pages workflow (`.github/workflows/pages.yml`); every push that
touches `web/**` redeploys automatically.

## Deploy anywhere

- **Every asset reference is relative** — no leading slashes, no domain
  assumptions, no CDN, no build step. The site works identically:
  - opened locally via `file://` (double-click `index.html`),
  - served from any static host,
  - hosted under any subfolder of any domain (e.g.
    `https://example.com/projects/kindboard/`).
- Upload the contents of this folder (index.html, styles.css, app.js,
  network.js, sw.js, favicon.svg, assets/) to any web root — nothing else is
  needed.

## Night / light theme

The site ships with a full night (dark) and light theme. A sun/moon toggle in
the header switches instantly, the choice persists in `localStorage`
(`kb-theme`), and the browser-tab `theme-color` follows. On first visit the
stored preference wins, otherwise the OS `prefers-color-scheme` is respected.

## Animated hero network

`network.js` draws a gentle particle network on the hero canvas: drifting
particles, proximity links, and a pointer that attracts and links nearby
particles. It respects `prefers-reduced-motion` (the static grid stays), only
animates while the hero is visible, and pauses when the tab is hidden.

## Offline

Over http(s), a service worker (`sw.js`) caches all assets on first visit, so
subsequent visits — and reloads after the server is gone — work fully offline.
`file://` needs no service worker: every asset is already a local file.

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
| `styles.css` | Brand-matched design system (teal-blue accent, dark and light themes) |
| `app.js` | Vanilla JS: mobile nav, scroll-spy, image lightbox, theme toggle, service-worker registration |
| `network.js` | Vanilla JS: animated hero particle network (respects `prefers-reduced-motion`) |
| `sw.js` | Service worker: caches all assets for offline use (http(s) only) |
| `favicon.svg` / `favicon-192.png` / `apple-touch-icon.png` | O.R.P mark in browser-tab and touch sizes |
| `assets/img/orp-mark.svg` | O.R.P mark only (transparent — used in the header next to the HTML wordmark) |
| `assets/img/logo-orp.svg` | O.R.P full lockup (transparent background, light text for dark surfaces — used in the footer) |
| `assets/img/screenshot-*.png` | Real app captures: overview ×3 themes, cluster ×2 themes, wizard |
| `og-image.png` | Social-share image (O.R.P mark) |
