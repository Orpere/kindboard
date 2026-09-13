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
})();
