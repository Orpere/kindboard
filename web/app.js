// kindboard website — minimal vanilla JS: mobile nav + image lightbox.
// No dependencies, no analytics, works from file:// or any server.

(function () {
  "use strict";

  // Mobile navigation toggle.
  var toggle = document.getElementById("nav-toggle");
  var nav = document.getElementById("site-nav");
  if (toggle && nav) {
    toggle.addEventListener("click", function () {
      var open = nav.classList.toggle("open");
      toggle.setAttribute("aria-expanded", open ? "true" : "false");
    });
    nav.querySelectorAll("a").forEach(function (link) {
      link.addEventListener("click", function () {
        nav.classList.remove("open");
        toggle.setAttribute("aria-expanded", "false");
      });
    });
  }

  // Highlight the section in the nav while scrolling.
  var sections = document.querySelectorAll("main section[id]");
  var navLinks = document.querySelectorAll(".site-nav a[href^='#']");
  if ("IntersectionObserver" in window && sections.length) {
    var current = "";
    var observer = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          if (entry.isIntersecting) {
            current = "#" + entry.target.id;
            navLinks.forEach(function (link) {
              link.classList.toggle("active", link.getAttribute("href") === current);
            });
          }
        });
      },
      { rootMargin: "-40% 0px -55% 0px" }
    );
    sections.forEach(function (section) {
      observer.observe(section);
    });
    document.querySelector(".site-nav").insertAdjacentHTML(
      "beforeend",
      "<style>.site-nav a.active { color: var(--accent); }</style>"
    );
  }

  // Screenshot lightbox.
  var lightbox = document.getElementById("lightbox");
  var lightboxImg = document.getElementById("lightbox-img");
  var lightboxClose = document.getElementById("lightbox-close");
  if (lightbox && lightboxImg) {
    document.querySelectorAll("[data-lightbox]").forEach(function (anchor) {
      anchor.addEventListener("click", function (event) {
        event.preventDefault();
        lightboxImg.src = anchor.getAttribute("href");
        lightboxImg.alt = anchor.querySelector("img")
          ? anchor.querySelector("img").alt
          : "";
        lightbox.classList.add("open");
        lightbox.setAttribute("aria-hidden", "false");
      });
    });
    var close = function () {
      lightbox.classList.remove("open");
      lightbox.setAttribute("aria-hidden", "true");
      lightboxImg.src = "";
    };
    if (lightboxClose) {
      lightboxClose.addEventListener("click", close);
    }
    lightbox.addEventListener("click", function (event) {
      if (event.target === lightbox) close();
    });
    document.addEventListener("keydown", function (event) {
      if (event.key === "Escape" && lightbox.classList.contains("open")) close();
    });
  }

  // Night / light theme toggle.
  var themeToggle = document.getElementById("theme-toggle");
  var metaTheme = document.querySelector('meta[name="theme-color"]');
  function applyTheme(theme, persist) {
    document.documentElement.setAttribute("data-theme", theme);
    if (metaTheme) metaTheme.setAttribute("content", theme === "light" ? "#f5f7fa" : "#12161c");
    if (themeToggle) {
      var light = theme === "light";
      themeToggle.setAttribute("aria-pressed", light ? "true" : "false");
      themeToggle.setAttribute("aria-label", light ? "Switch to night theme" : "Switch to light theme");
      themeToggle.setAttribute("title", light ? "Switch to night theme" : "Switch to light theme");
    }
    if (persist) {
      try { localStorage.setItem("kb-theme", theme); } catch (e) {}
    }
  }
  var currentTheme = document.documentElement.getAttribute("data-theme") || "dark";
  applyTheme(currentTheme === "light" ? "light" : "dark", false);
  if (themeToggle) {
    themeToggle.addEventListener("click", function () {
      var theme = document.documentElement.getAttribute("data-theme") === "light" ? "dark" : "light";
      applyTheme(theme, true);
      document.dispatchEvent(new CustomEvent("kb-theme-change", { detail: { theme: theme } }));
    });
  }

  // OS-aware download button (resolver lives in download.js, loaded first).
  // Failure is always a no-op: the button keeps its no-JS default href, which
  // points at the releases page — never a wrong binary.
  function wireDownloadButton() {
    var btn = document.getElementById("download-btn");
    if (!btn || !window.kindboardDownload) {
      return;
    }
    var resolved;
    try {
      var nav = window.navigator || {};
      resolved = window.kindboardDownload.resolve({
        userAgent: nav.userAgent || "",
        platform: nav.platform || "",
        userAgentDataPlatform: (nav.userAgentData && nav.userAgentData.platform) || ""
      });
    } catch (e) {
      return;
    }
    if (!resolved || !resolved.primary || resolved.fallback) {
      return;
    }
    btn.href = resolved.primary.href;
    btn.textContent = resolved.primary.label;
    btn.setAttribute("rel", "noopener");
    var alt = document.getElementById("download-btn-alt");
    if (resolved.alt && alt) {
      alt.href = resolved.alt.href;
      alt.textContent = resolved.alt.label;
      alt.setAttribute("rel", "noopener");
      alt.removeAttribute("hidden");
    }
  }
  wireDownloadButton();

  // Offline support: register the service worker on http(s) hosts.
  // file:// has no service worker support, but the site is already
  // fully self-contained there (all assets are local files).
  window.addEventListener("load", function () {
    if ("serviceWorker" in navigator && location.protocol !== "file:") {
      var swUrl = new URL("sw.js", document.baseURI).href;
      navigator.serviceWorker.register(swUrl).catch(function () {});
    }
  });
})();
