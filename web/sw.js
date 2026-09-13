// kindboard website — service worker: offline cache of static assets.
// Plain ES5-style script; cache-first for same-origin GET requests.

// Cache name is scoped to this service worker's scope path, so several
// kindboard deployments (or other projects) sharing one origin each keep
// their own cache and this SW never deletes anything outside its scope.
var SCOPE_PATH = new URL(self.registration.scope).pathname;
var CACHE_PREFIX = "kindboard-site-v3:";
var CACHE = CACHE_PREFIX + SCOPE_PATH;
var ASSETS = [
  "./",
  "index.html",
  "styles.css",
  "app.js",
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
  event.respondWith(
    caches.match(request).then(function (cached) {
      if (cached) {
        // Cache-first: refresh the cached copy in the background.
        fetch(request).then(function (response) {
          if (response && response.status === 200 && response.type === "basic") {
            caches.open(CACHE).then(function (cache) {
              cache.put(request, response.clone());
            });
          }
        }).catch(function () {});
        return cached;
      }
      return fetch(request).then(function (response) {
        if (response && response.status === 200 && response.type === "basic") {
          caches.open(CACHE).then(function (cache) {
            cache.put(request, response.clone());
          });
        }
        return response;
      }).catch(function () {
        if (request.mode === "navigate") {
          return caches.match("./");
        }
        throw new Error("offline: " + request.url);
      });
    })
  );
});
