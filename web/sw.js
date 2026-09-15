// kindboard website — service worker.
//
// Strategy (fixes the "stale site after a release" problem):
//   - Navigations and the files that change on every release
//     (index.html, styles.css, app.js, download.js, network.js):
//     NETWORK-FIRST with cache fallback — online visitors always get the
//     current page, and every successful fetch refreshes the offline cache.
//     No manual cache-version bump is needed per release anymore.
//   - Everything else (images, favicons): CACHE-FIRST with a background
//     refresh (stale-while-revalidate) — heavy, rarely-changed assets load
//     instantly. Screenshots are in this set, so when they are regenerated
//     with the SAME filenames (e.g. a theme-visual refresh), bump
//     CACHE_PREFIX below — otherwise returning visitors keep the stale copy
//     until the background refresh completes.
// Offline: the cached page + assets still serve fully after the first visit.
// Plain ES5-style script.

// Cache name is scoped to this service worker's scope path, so several
// kindboard deployments (or other projects) sharing one origin each keep
// their own cache and this SW never deletes anything outside its scope.
var SCOPE_PATH = new URL(self.registration.scope).pathname;
var CACHE_PREFIX = "kindboard-site-v11:";
var CACHE = CACHE_PREFIX + SCOPE_PATH;
var ASSETS = [
  "./",
  "index.html",
  "styles.css",
  "app.js",
  "download.js",
  "network.js",
  "favicon.svg",
  "favicon-192.png",
  "apple-touch-icon.png",
  "og-image.png",
  "assets/img/orp-mark.svg",
  "assets/img/logo-orp.svg",
  "assets/img/screenshot-cluster-dark.png",
  "assets/img/screenshot-cluster-light.png",
  "assets/img/screenshot-overview-dark.png",
  "assets/img/screenshot-overview-light.png",
  "assets/img/screenshot-overview-high-contrast.png",
  "assets/img/screenshot-wizard-dark.png"
];
// Files that must never be served stale while online (change every release).
var FRESH = ["index.html", "styles.css", "app.js", "download.js", "network.js"];

function isUsableResponse(response) {
  return !!response && response.status === 200 && response.type === "basic";
}

function putInCache(request, response) {
  caches.open(CACHE).then(function (cache) {
    cache.put(request, response.clone());
  });
}

// Network-first: fresh content when online, cached copy when offline.
function networkFirst(request) {
  return fetch(request).then(function (response) {
    if (isUsableResponse(response)) {
      putInCache(request, response);
      // Keep the offline navigation entry in sync with the current page.
      if (request.mode === "navigate") {
        caches.open(CACHE).then(function (cache) {
          cache.put("./", response.clone());
        });
      }
    }
    return response;
  }).catch(function () {
    return caches.match(request).then(function (cached) {
      if (cached) {
        return cached;
      }
      if (request.mode === "navigate") {
        return caches.match("./");
      }
      throw new Error("offline: " + request.url);
    });
  });
}

// Cache-first with a background refresh (stale-while-revalidate).
function cacheFirstRefresh(request) {
  return caches.match(request).then(function (cached) {
    if (cached) {
      fetch(request).then(function (response) {
        if (isUsableResponse(response)) {
          putInCache(request, response);
        }
      }).catch(function () {});
      return cached;
    }
    return fetch(request).then(function (response) {
      if (isUsableResponse(response)) {
        putInCache(request, response);
      }
      return response;
    }).catch(function () {
      throw new Error("offline: " + request.url);
    });
  });
}

self.addEventListener("install", function (event) {
  event.waitUntil(
    caches.open(CACHE).then(function (cache) {
      return cache.addAll(ASSETS);
    }).then(function () {
      return self.skipWaiting();
    }).catch(function (err) {
      // One bad asset must not block the install.
      console.error("kindboard service worker: install cache failed", err);
    })
  );
});

self.addEventListener("activate", function (event) {
  event.waitUntil(
    caches.keys().then(function (keys) {
      // Only retire older caches of THIS site (same prefix) and only when
      // the current cache actually holds content — a failed install must
      // never wipe a previously working offline copy.
      return caches.has(CACHE).then(function (hasCurrent) {
        if (!hasCurrent) {
          return Promise.resolve();
        }
        return Promise.all(keys.filter(function (key) {
          return key.indexOf(CACHE_PREFIX) === 0 && key !== CACHE;
        }).map(function (key) {
          return caches.delete(key);
        }));
      });
    }).then(function () {
      return self.clients.claim();
    })
  );
});

self.addEventListener("fetch", function (event) {
  var request = event.request;
  if (request.method !== "GET") {
    return;
  }
  var url;
  try {
    url = new URL(request.url);
  } catch (e) {
    return;
  }
  if (url.origin !== self.location.origin) {
    return;
  }

  var name = url.pathname.split("/").pop();
  if (request.mode === "navigate" || FRESH.indexOf(name) !== -1) {
    event.respondWith(networkFirst(request));
    return;
  }
  event.respondWith(cacheFirstRefresh(request));
});
