// kindboard website — animated hero particle network.
// Vanilla canvas, zero dependencies. Respects prefers-reduced-motion,
// pauses when hidden or off-screen, and recolors with the site theme.

(function () {
  "use strict";

  var hero = document.querySelector(".hero");
  var canvas = document.getElementById("hero-network");
  if (!hero || !canvas) {
    return;
  }
  if (window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
    return;
  }

  var ctx = canvas.getContext("2d");
  if (!ctx) {
    return;
  }

  canvas.style.position = "absolute";
  canvas.style.inset = "0";
  canvas.style.pointerEvents = "none";
  canvas.style.zIndex = "1";

  var LINK_DIST = 110;
  var POINTER_DIST = 150;
  var MAX_SPEED = 0.35;
  var DAMPING = 0.985;

  var colors = {
    accent: "#3da5d9",
    neutral: "#2c3642",
    accentRgb: "61,165,217",
    neutralRgb: "44,54,66",
    linkRgb: "154,166,180"
  };
  var linkStyle = "rgba(154,166,180,1)";
  var accentStyle = "rgba(61,165,217,1)";
  var neutralStyle = "rgba(44,54,66,1)";
  var dpr = 1;
  var width = 0;
  var height = 0;
  var particles = [];
  var count = 0;
  var pointer = null;
  var running = false;
  var rafId = null;
  var heroRect = { left: 0, top: 0 };

  function hexToRgb(hex) {
    var h = hex.replace("#", "");
    if (h.length === 3) {
      h = h.charAt(0) + h.charAt(0) + h.charAt(1) + h.charAt(1) + h.charAt(2) + h.charAt(2);
    }
    if (h.length !== 6) {
      return null;
    }
    var r = parseInt(h.substring(0, 2), 16);
    var g = parseInt(h.substring(2, 4), 16);
    var b = parseInt(h.substring(4, 6), 16);
    if (isNaN(r) || isNaN(g) || isNaN(b)) {
      return null;
    }
    return r + "," + g + "," + b;
  }

  function readColors() {
    var styles = getComputedStyle(document.documentElement);
    var accent = styles.getPropertyValue("--accent").trim();
    var neutral = styles.getPropertyValue("--stroke").trim();
    var link = styles.getPropertyValue("--text-dim").trim();
    colors.accent = accent || "#3da5d9";
    colors.neutral = neutral || "#2c3642";
    colors.accentRgb = hexToRgb(colors.accent) || "61,165,217";
    colors.neutralRgb = hexToRgb(colors.neutral) || "44,54,66";
    colors.linkRgb = hexToRgb(link) || "154,166,180";
    linkStyle = "rgba(" + colors.linkRgb + ",1)";
    accentStyle = "rgba(" + colors.accentRgb + ",1)";
    neutralStyle = "rgba(" + colors.neutralRgb + ",1)";
  }

  function syncRect() {
    var rect = hero.getBoundingClientRect();
    heroRect.left = rect.left;
    heroRect.top = rect.top;
  }

  function resize() {
    dpr = Math.min(window.devicePixelRatio || 1, 2);
    width = hero.clientWidth;
    height = hero.clientHeight;
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(height * dpr);
    canvas.style.width = width + "px";
    canvas.style.height = height + "px";
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    syncRect();

    var area = width * height;
    var next = Math.round(area / 11000);
    next = Math.max(24, Math.min(80, next));
    if (next !== count) {
      count = next;
      particles = [];
      for (var i = 0; i < count; i++) {
        particles.push({
          x: Math.random() * width,
          y: Math.random() * height,
          vx: (Math.random() * 2 - 1) * 0.15,
          vy: (Math.random() * 2 - 1) * 0.15,
          r: 1.2 + Math.random()
        });
      }
    }
    readColors();
  }

  function frame() {
    ctx.clearRect(0, 0, width, height);

    var i, j;
    var p, q;
    var dx, dy, d2, d, alpha;

    for (i = 0; i < count; i++) {
      p = particles[i];
      p.x += p.vx;
      p.y += p.vy;
      p.vx *= DAMPING;
      p.vy *= DAMPING;
      if (p.x < -10) p.x = width + 10;
      else if (p.x > width + 10) p.x = -10;
      if (p.y < -10) p.y = height + 10;
      else if (p.y > height + 10) p.y = -10;
    }

    var linkMax = LINK_DIST * LINK_DIST;
    for (i = 0; i < count; i++) {
      p = particles[i];
      for (j = i + 1; j < count; j++) {
        q = particles[j];
        dx = q.x - p.x;
        dy = q.y - p.y;
        d2 = dx * dx + dy * dy;
        if (d2 < linkMax) {
          d = Math.sqrt(d2);
          alpha = (1 - d / LINK_DIST) * 0.35;
          ctx.strokeStyle = linkStyle;
          ctx.globalAlpha = alpha;
          ctx.beginPath();
          ctx.moveTo(p.x, p.y);
          ctx.lineTo(q.x, q.y);
          ctx.stroke();
        }
      }
    }
    ctx.globalAlpha = 1;

    var inside = pointer !== null &&
      pointer.x >= 0 && pointer.x <= width &&
      pointer.y >= 0 && pointer.y <= height;

    if (inside) {
      var px = pointer.x;
      var py = pointer.y;
      var pointerMax = POINTER_DIST * POINTER_DIST;
      for (i = 0; i < count; i++) {
        p = particles[i];
        dx = px - p.x;
        dy = py - p.y;
        d2 = dx * dx + dy * dy;
        if (d2 < pointerMax) {
          d = Math.sqrt(d2);
          alpha = (1 - d / POINTER_DIST) * 0.5;
          ctx.strokeStyle = accentStyle;
          ctx.globalAlpha = alpha;
          ctx.beginPath();
          ctx.moveTo(p.x, p.y);
          ctx.lineTo(px, py);
          ctx.stroke();

          p.vx += dx * (1 - d / POINTER_DIST) * 0.00008;
          p.vy += dy * (1 - d / POINTER_DIST) * 0.00008;
          if (p.vx > MAX_SPEED) p.vx = MAX_SPEED;
          else if (p.vx < -MAX_SPEED) p.vx = -MAX_SPEED;
          if (p.vy > MAX_SPEED) p.vy = MAX_SPEED;
          else if (p.vy < -MAX_SPEED) p.vy = -MAX_SPEED;
        }
      }
      ctx.globalAlpha = 1;
    }

    ctx.fillStyle = accentStyle;
    ctx.globalAlpha = 0.9;
    for (i = 0; i < count; i++) {
      p = particles[i];
      ctx.beginPath();
      ctx.arc(p.x, p.y, p.r, 0, Math.PI * 2);
      ctx.fill();
    }

    if (inside) {
      ctx.fillStyle = neutralStyle;
      ctx.globalAlpha = 0.35;
      ctx.beginPath();
      ctx.arc(pointer.x, pointer.y, 9, 0, Math.PI * 2);
      ctx.fill();
      ctx.fillStyle = accentStyle;
      ctx.globalAlpha = 0.9;
      ctx.beginPath();
      ctx.arc(pointer.x, pointer.y, 3.5, 0, Math.PI * 2);
      ctx.fill();
    }

    ctx.globalAlpha = 1;

    if (running) {
      rafId = window.requestAnimationFrame(frame);
    }
  }

  function start() {
    if (running) {
      return;
    }
    running = true;
    rafId = window.requestAnimationFrame(frame);
  }

  function stop() {
    running = false;
    if (rafId !== null) {
      window.cancelAnimationFrame(rafId);
      rafId = null;
    }
  }

  function clearPointer() {
    pointer = null;
  }

  window.addEventListener("pointermove", function (event) {
    if (pointer === null) {
      pointer = { x: 0, y: 0 };
    }
    pointer.x = event.clientX - heroRect.left;
    pointer.y = event.clientY - heroRect.top;
  });
  window.addEventListener("pointerleave", clearPointer);
  window.addEventListener("pointerup", clearPointer);
  window.addEventListener("pointercancel", clearPointer);
  hero.addEventListener("pointerleave", clearPointer);

  document.addEventListener("visibilitychange", function () {
    if (document.hidden) {
      stop();
    } else {
      start();
    }
  });

  if ("IntersectionObserver" in window) {
    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          start();
        } else {
          stop();
        }
      });
    });
    observer.observe(hero);
  }

  window.addEventListener("scroll", syncRect, { passive: true });
  document.addEventListener("kb-theme-change", readColors);
  window.addEventListener("resize", resize);

  hero.classList.add("has-network");
  resize();
  start();
})();
