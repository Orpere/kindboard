//! Host-curl capability probing for HTTPS-only download hardening.
//!
//! kindboard shells out to the *host* `curl` (never a bundled copy) for its
//! binary tool downloads and provisioned-manifest fetches. The flags this
//! module emits use the **restrictive `=` modifier** — `--proto '=https'` and
//! `--proto-redir '=https'` (two argv tokens each) — which *genuinely*
//! enforce HTTPS-only transport for the initial URL and any redirect target:
//! `--proto=https` (the old, non-`=` form) is a no-op that merely *adds*
//! https to the already-allowed set. They are still *defense-in-depth*: the
//! pinned SHA-256 digest verification that runs immediately after every
//! download is the mandatory integrity anchor, and it fails closed on
//! mismatch even when the flags are absent.
//!
//! Not every curl is equal. Old macOS system curl, BusyBox/minimal builds and
//! stub curls shipped first on `PATH` predate (or simply lack) `--proto`, and
//! curl rejects an unknown option with exit code 2 before doing anything else.
//! On such hosts the hardcoded flags abort the download with
//! `curl: option --proto: is unknown`, which is exactly the failure this
//! module removes: it probes once, then downgrades the plan to plain curl on
//! degraded hosts while keeping digest verification as the fail-closed anchor.

use std::process::{Command, Stdio};

#[cfg(test)]
use std::sync::Mutex;
#[cfg(not(test))]
use std::sync::OnceLock;

/// Which HTTPS-hardening curl flags the host curl actually supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurlOpts {
    /// `curl --proto '=https' …` is accepted (HTTPS-only initial URL).
    pub proto: bool,
    /// `curl --proto-redir '=https' …` is accepted (HTTPS-only redirects).
    pub proto_redir: bool,
}

impl CurlOpts {
    /// Both flags supported — the common case on modern curl (Homebrew
    /// macOS, current Linux distros, Windows' bundled `curl.exe`).
    pub const ALL: CurlOpts = CurlOpts {
        proto: true,
        proto_redir: true,
    };

    /// Neither flag supported — degraded hosts (old macOS system curl,
    /// BusyBox/minimal curls first on `PATH`).
    pub const NONE: CurlOpts = CurlOpts {
        proto: false,
        proto_redir: false,
    };
}

/// Probe `program` for `--proto '=https'` and `--proto-redir '=https'`
/// support.
///
/// The probe leans on a verified fact: curl parses the full argv **before**
/// honoring `--version`, so an unknown option makes it exit 2 (printing
/// `curl: option <arg>: is unknown`) without ever reaching `--version`. A
/// zero exit therefore proves the option is recognized — no output parsing
/// and no network access. Each flag is probed as two argv tokens
/// (`--proto`, `=https`), matching how it is later emitted. `--proto-redir`
/// is probed only after `--proto` succeeds.
///
/// Never panics: a spawn failure (missing or unnameable binary) reads as both
/// flags false.
pub fn probe_program(program: &str) -> CurlOpts {
    let proto = probe_flag(program, "--proto");
    let proto_redir = proto && probe_flag(program, "--proto-redir");
    CurlOpts { proto, proto_redir }
}

/// Run `<program> <flag> =https --version` and report whether it exited 0.
fn probe_flag(program: &str, flag: &str) -> bool {
    match Command::new(program)
        .arg(flag)
        .arg("=https")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

/// The host curl program's capabilities (default probe target).
///
/// Only used in non-test builds (test builds default to [`CurlOpts::ALL`]
/// via [`curl_caps`]).
#[cfg(not(test))]
fn probe() -> CurlOpts {
    probe_program(if cfg!(windows) { "curl.exe" } else { "curl" })
}

#[cfg(not(test))]
static CAPS: OnceLock<CurlOpts> = OnceLock::new();
#[cfg(test)]
static CAPS: Mutex<Option<CurlOpts>> = Mutex::new(None);

/// The host curl's [`CurlOpts`], memoized for the process lifetime.
///
/// In test builds this defaults to [`CurlOpts::ALL`] (and is resettable via
/// `set_test_caps`): the 24 existing `plan_install_for` tests assert plan
/// shapes built with the flags present, and a real `curl` probe would make
/// them environment-dependent. The test-mode default keeps them hermetic and
/// green with zero churn; new tests should prefer the explicit `_with` API in
/// `deps` to avoid cross-test races.
pub fn curl_caps() -> CurlOpts {
    #[cfg(not(test))]
    {
        *CAPS.get_or_init(probe)
    }
    #[cfg(test)]
    {
        CAPS.lock().unwrap().unwrap_or(CurlOpts::ALL)
    }
}

/// Test-only override for [`curl_caps`] (resets the memoized value).
#[cfg(test)]
pub fn set_test_caps(caps: Option<CurlOpts>) {
    *CAPS.lock().unwrap() = caps;
}

/// The curl `--proto`/`--proto-redir` args to use for the given capabilities.
///
/// Emits the restrictive two-token form: `["--proto", "=https"]` and/or
/// `["--proto-redir", "=https"]` according to what the host curl supports, so
/// a degraded host downgrades to plain curl instead of aborting on an unknown
/// option, while a capable host genuinely enforces HTTPS-only transport.
pub fn secure_args(caps: CurlOpts) -> Vec<&'static str> {
    let mut args = Vec::with_capacity(4);
    if caps.proto {
        args.push("--proto");
        args.push("=https");
    }
    if caps.proto_redir {
        args.push("--proto-redir");
        args.push("=https");
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::path::{Path, PathBuf};

    #[cfg(unix)]
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kindboard-curl-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn make_shim(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    #[cfg(unix)]
    #[test]
    fn probe_program_reports_full_support_for_zero_exit_shim() {
        let dir = temp_dir("probe-full");
        let shim = make_shim(&dir, "curl", "#!/bin/sh\nexit 0\n");
        assert_eq!(probe_program(shim.to_str().unwrap()), CurlOpts::ALL);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn probe_program_reports_none_when_proto_is_rejected() {
        let dir = temp_dir("probe-none");
        // curl's unknown-option behavior: exit 2 when the argv contains
        // `--proto` immediately followed by `=https` (the two-token
        // restrictive form), else 0. The first probe is `--proto =https
        // --version`, so it fails and `--proto-redir` is never probed → NONE.
        let shim = make_shim(
            &dir,
            "curl",
            "#!/bin/sh\nwhile [ $# -gt 1 ]; do if [ \"$1\" = \"--proto\" ] && [ \"$2\" = \"=https\" ]; then exit 2; fi; shift; done\nexit 0\n",
        );
        assert_eq!(probe_program(shim.to_str().unwrap()), CurlOpts::NONE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn probe_program_reports_proto_only_when_proto_redir_is_rejected() {
        let dir = temp_dir("probe-partial");
        // Accepts `--proto =https` (exits 0) but rejects `--proto-redir
        // =https`.
        let shim = make_shim(
            &dir,
            "curl",
            "#!/bin/sh\nwhile [ $# -gt 1 ]; do if [ \"$1\" = \"--proto-redir\" ] && [ \"$2\" = \"=https\" ]; then exit 2; fi; shift; done\nexit 0\n",
        );
        assert_eq!(
            probe_program(shim.to_str().unwrap()),
            CurlOpts {
                proto: true,
                proto_redir: false,
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Windows shim probing is intentionally not exercised here: the repo is
    // CI-less and tests run on Linux. `probe_program` is platform-neutral and
    // the `curl.exe` branch of `probe()` is covered by compilation.

    #[test]
    fn secure_args_reflects_caps() {
        assert_eq!(
            secure_args(CurlOpts::ALL),
            vec!["--proto", "=https", "--proto-redir", "=https"]
        );
        assert_eq!(secure_args(CurlOpts::NONE), Vec::<&'static str>::new());
        assert_eq!(
            secure_args(CurlOpts {
                proto: true,
                proto_redir: false,
            }),
            vec!["--proto", "=https"]
        );
        assert_eq!(
            secure_args(CurlOpts {
                proto: false,
                proto_redir: true,
            }),
            vec!["--proto-redir", "=https"]
        );
    }

    #[test]
    fn set_test_caps_override_is_honored() {
        // No existing test asserts proto-flag presence, so a concurrent
        // reader observing NONE cannot break other tests (no serialization
        // crate needed).
        set_test_caps(Some(CurlOpts::NONE));
        assert_eq!(curl_caps(), CurlOpts::NONE);
        set_test_caps(None);
        assert_eq!(curl_caps(), CurlOpts::ALL);
    }
}
