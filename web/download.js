// kindboard website — OS-agnostic download resolution (pure logic, no DOM).
// Exposed as window.kindboardDownload for the site and as a CommonJS module
// for Node tests. ES5 style like the rest of the site: no dependencies.
//
// Why: the hero Download button used to hardcode the Linux tarball. A macOS
// user clicking it downloaded a Linux ELF, and running it printed
// `zsh: exec format error`. The resolver maps the visitor's platform to the
// matching release artifact, and macOS is presented as two equal choices
// (Apple Silicon / Intel) — browsers cannot reliably report the Mac's CPU
// arch, so guessing a default would reintroduce the same bug for Intel Macs.

(function (global) {
  "use strict";

  var VERSION = "v0.1.18";
  var RELEASES = "https://github.com/Orpere/kindboard/releases";
  var ARTIFACTS = {
    linux: RELEASES + "/download/" + VERSION + "/kindboard-linux-x86_64.tar.gz",
    macArm: RELEASES + "/download/" + VERSION + "/kindboard-darwin-arm64.tar.gz",
    macX64: RELEASES + "/download/" + VERSION + "/kindboard-darwin-x86_64.tar.gz",
    windows: RELEASES + "/download/" + VERSION + "/kindboard-windows-x86_64.zip"
  };
  // No-JS / unknown-platform fallback: the releases page is never a wrong
  // binary. Callers should leave the button untouched when `fallback` is
  // true, so the default href (releases/latest) stays in place.
  var FALLBACK = { href: RELEASES + "/latest", label: "Download " + VERSION };

  /**
   * resolveDownload(env) -> { primary: {href, label}, alt: {href,label}|null,
   *                          fallback: boolean }
   * env: { userAgent: string, platform: string,
   *        userAgentDataPlatform: string|undefined }
   *
   * Detection order (first match wins):
   *   1. Windows  — userAgentData.platform "Windows" or navigator.platform "Win*"
   *   2. macOS    — platform "MacIntel" / uaData "macOS" / UA "Macintosh"
   *                 → primary Apple Silicon, alt Intel (no arch default)
   *   3. Linux    — platform "Linux*" or UA contains "Linux"
   *   4. fallback — releases page
   */
  function resolveDownload(env) {
    env = env || {};
    var ua = String(env.userAgent || "");
    var platform = String(env.platform || "");
    var uadp = String(env.userAgentDataPlatform || "");

    if (uadp === "Windows" || platform.indexOf("Win") === 0) {
      return {
        primary: { href: ARTIFACTS.windows, label: "Download " + VERSION + " (Windows x64)" },
        alt: null,
        fallback: false
      };
    }
    if (platform === "MacIntel" || uadp === "macOS" || ua.indexOf("Macintosh") !== -1) {
      return {
        primary: { href: ARTIFACTS.macArm, label: "Download " + VERSION + " — macOS Apple Silicon" },
        alt: { href: ARTIFACTS.macX64, label: "macOS Intel" },
        fallback: false
      };
    }
    if (platform.indexOf("Linux") !== -1 || ua.indexOf("Linux") !== -1) {
      return {
        primary: { href: ARTIFACTS.linux, label: "Download " + VERSION + " (Linux x86_64)" },
        alt: null,
        fallback: false
      };
    }
    return { primary: FALLBACK, alt: null, fallback: true };
  }

  var api = {
    resolve: resolveDownload,
    version: VERSION,
    fallback: FALLBACK
  };

  // Browser: expose for app.js (loaded right after this file).
  global.kindboardDownload = api;

  // Node (tests): export the API. Browsers never see a `module` global.
  if (typeof module !== "undefined" && module.exports) {
    module.exports = api;
  }
})(typeof window !== "undefined" ? window : this);
