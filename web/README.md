# kindboard website (`web/`)

Static, self-contained presentation site for kindboard.

**Live:** <https://orpere.github.io/kindboard/> — published from this folder
with `make publish` (repo root): it pushes `web/**` to the `gh-pages` branch
and switches GitHub Pages to legacy branch publishing. No CI, no GitHub
Actions — deploys run locally from the Makefile.

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

Caching strategy (stale releases must never stick):

- **Navigations + files that change every release** (`index.html`,
  `styles.css`, `app.js`, `download.js`, `network.js`): **network-first** with
  cache fallback. Online visitors always get the current page, and each
  successful fetch refreshes the offline copy — no manual service-worker cache
  bump is required when shipping a release.
- **Everything else** (screenshots, favicons): **cache-first** with a
  background refresh. Heavy assets load instantly; new screenshot filenames
  are never stale.

## Publishing

From the repo root: edit `web/*`, commit, then `make publish`. It copies this
folder into a scratch git repo, force-pushes it to the `gh-pages` branch, and
points GitHub Pages at that branch (`build_type=legacy`, no Actions). All
links and references in this repo are already final — no placeholders.

## Files

| Path | Purpose |
|---|---|
| `index.html` | Single-page site (hero, features, architecture, components, screenshots, themes, CNI matrix, getting started, roadmap) |
| `styles.css` | Brand-matched design system (teal-blue accent, dark and light themes) |
| `app.js` | Vanilla JS: mobile nav, scroll-spy, image lightbox, theme toggle, service-worker registration |
| `network.js` | Vanilla JS: animated hero particle network (respects `prefers-reduced-motion`) |
| `sw.js` | Service worker: caches all assets for offline use (http(s) only) |
| `favicon.svg` / `favicon-192.png` / `apple-touch-icon.png` | O.R.P mark in browser-tab and touch sizes |
| `assets/img/orp-mark.svg` | O.R.P mark only (transparent — used in the header next to the HTML wordmark) |
| `assets/img/logo-orp.svg` | O.R.P full lockup (transparent background, light text for dark surfaces — used in the footer) |
| `assets/img/screenshot-*.png` | Real app captures: overview ×3 themes, cluster ×2 themes, wizard |
| `og-image.png` | Social-share image (O.R.P mark) |
