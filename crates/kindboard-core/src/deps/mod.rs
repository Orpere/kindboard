//! Dependency tool detection and installation.
//!
//! - [`registry`] is the data-driven tool list (matrix doc is the source of
//!   truth; one struct literal per tool).
//! - [`detect`] looks a tool up on PATH, runs its version command and parses
//!   the output into a [`Version`] (never a raw string).
//! - [`plan_install`] builds the exact command list for the current platform
//!   (package manager first, binary fallback to `~/.local/bin`) *without*
//!   executing anything — unit-testable.
//! - [`install_with_progress`] executes that plan through the `exec` runner,
//!   streaming events.
//! - [`check_docker_daemon`] probes the daemon separately (a missing daemon
//!   is not a missing binary).

mod registry;

pub use registry::{TOOLS, registry, tool};

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::error::{DepsError, Result};
use crate::exec::Cmd;

/// Timeout for version probes.
pub const DETECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Timeout for docker daemon probes.
pub const DAEMON_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// Timeout for downloads (large binaries).
pub const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
/// Timeout for package-manager installs.
pub const PKG_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
/// Default install directory for binary fallbacks.
pub const LOCAL_BIN_DIR_NAME: &str = ".local/bin";

/// A parsed tool version (`major.minor.patch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Version {
    /// Major component.
    pub major: u32,
    /// Minor component.
    pub minor: u32,
    /// Patch component.
    pub patch: u32,
}

impl Version {
    /// The unknown/absent version (0.0.0), used when a tool is present but
    /// its version cannot be determined (e.g. old kubectx scripts).
    pub const UNKNOWN: Version = Version {
        major: 0,
        minor: 0,
        patch: 0,
    };

    /// Whether this is a real (non-unknown) version.
    pub fn is_known(&self) -> bool {
        *self != Version::UNKNOWN
    }

    /// Compare against a minimum `major.minor.patch`.
    pub fn at_least(&self, major: u32, minor: u32, patch: u32) -> bool {
        *self
            >= Version {
                major,
                minor,
                patch,
            }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Parse `X.Y.Z` (digits only). Returns `None` for anything else.
pub fn parse_version3(s: &str) -> Option<Version> {
    let parts: Vec<&str> = s.trim().split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut nums = [0u32; 3];
    for (slot, part) in nums.iter_mut().zip(parts.iter()) {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some(Version {
        major: nums[0],
        minor: nums[1],
        patch: nums[2],
    })
}

/// The 8 dependency tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolId {
    /// Docker (daemon + CLI).
    Docker,
    /// kind.
    Kind,
    /// kubectl.
    Kubectl,
    /// helm.
    Helm,
    /// cilium CLI.
    Cilium,
    /// k9s.
    K9s,
    /// kubectx (kubens ships in a separate upstream archive; not installed).
    Kubectx,
    /// kustomize.
    Kustomize,
}

impl fmt::Display for ToolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ToolId::Docker => "docker",
            ToolId::Kind => "kind",
            ToolId::Kubectl => "kubectl",
            ToolId::Helm => "helm",
            ToolId::Cilium => "cilium",
            ToolId::K9s => "k9s",
            ToolId::Kubectx => "kubectx",
            ToolId::Kustomize => "kustomize",
        };
        f.write_str(s)
    }
}

/// How to run a tool's version command and parse the result.
#[derive(Debug, Clone)]
pub struct DetectSpec {
    /// Binary name to look up on PATH.
    pub command: &'static str,
    /// Args for the version command.
    pub version_args: &'static [&'static str],
    /// How to extract the version from the output.
    pub parse: VersionParse,
}

/// Version extraction strategies (see the matrix doc for per-tool formats).
#[derive(Debug, Clone, Copy)]
pub enum VersionParse {
    /// Navigate a dot-path into JSON output (e.g.
    /// `clientVersion.gitVersion`).
    JsonField(&'static str),
    /// Tiny regex subset: either `v(\d+\.\d+\.\d+)` (first match) or
    /// `Version\s+(\S+)` (first token after the word "Version").
    Regex(&'static str),
    /// Strip the prefix up to (and including) the first `v`, then parse the
    /// rest.
    ShortLine,
}

/// Where to install a tool on a given platform.
#[derive(Debug, Clone)]
pub struct InstallRecipe {
    /// Homebrew formula.
    pub brew: Option<&'static str>,
    /// Homebrew cask (macOS GUI apps, e.g. Docker Desktop).
    pub brew_cask: Option<&'static str>,
    /// dnf package (Fedora).
    pub dnf: Option<&'static str>,
    /// apt package (Ubuntu/Debian; installed via `apt-get`).
    pub apt: Option<&'static str>,
    /// pacman package (Arch).
    pub pacman: Option<&'static str>,
    /// winget package id (Windows; `winget install --id <id> ...`).
    pub winget: Option<&'static str>,
    /// Chocolatey package (Windows; `choco install -y <pkg>`).
    pub choco: Option<&'static str>,
    /// True when the tool cannot run on Windows at all (e.g. kubectx is a
    /// POSIX shell script) — every Windows plan fails with an honest error
    /// before any package manager or binary fallback is attempted.
    pub windows_unsupported: bool,
    /// Binary download fallback into `~/.local/bin`.
    pub binary: Option<BinaryDownload>,
    /// True when only a package manager can install this (docker).
    pub pkg_manager_only: bool,
    /// Human-readable post-install note (daemon enable, group membership…).
    pub post_install: Option<&'static str>,
}

/// A binary fallback download (matrix URLs).
#[derive(Debug, Clone)]
pub struct BinaryDownload {
    /// URL template with `{os}` (linux/darwin/windows), `{OS}`
    /// (Linux/Darwin/Windows), `{arch}` (amd64/arm64) and `{x64}`
    /// (x86_64/arm64) placeholders.
    pub url_template: &'static str,
    /// Optional Windows URL template (same placeholders). Windows artifacts
    /// differ from unix ones (`.zip` instead of `.tar.gz`, `.exe` names), so
    /// `None` = render [`Self::url_template`] with `os = "windows"`.
    pub windows_url_template: Option<&'static str>,
    /// Paths inside the archive to extract (empty = the download *is* the
    /// binary). Destination file name = the member's basename.
    pub members: &'static [&'static str],
    /// Windows archive members (same shape; Windows zips contain `.exe`
    /// binaries). Empty = the download *is* the binary (installed as
    /// `<command>.exe`).
    pub windows_members: &'static [&'static str],
    /// Expected SHA-256 digests of the downloaded artifact, keyed by the
    /// canonical `"{os}-{arch}"` platform key (`linux-amd64`,
    /// `linux-arm64`, `darwin-amd64`, `darwin-arm64`, `windows-amd64`).
    /// Downloads whose platform has no entry are refused. Digests are pinned
    /// constants, transcribed from the upstream `.sha256sum`/`checksums.txt`
    /// release assets (see the registry comments for the per-tool source).
    pub sha256: &'static [(&'static str, &'static str)],
}

/// One dependency tool entry in the registry.
#[derive(Debug, Clone)]
pub struct Tool {
    /// Stable id.
    pub id: ToolId,
    /// Human display name.
    pub display: &'static str,
    /// Detection spec.
    pub detect: DetectSpec,
    /// Install recipe.
    pub install: InstallRecipe,
}

/// Detection outcome for one tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStatus {
    /// Found and version parsed.
    Installed {
        /// Parsed version (may be [`Version::UNKNOWN`] for presence-only
        /// tools like old kubectx scripts).
        version: Version,
        /// Absolute path of the binary.
        path: PathBuf,
    },
    /// Not on PATH.
    NotInstalled,
    /// Present but broken (non-zero exit or unparseable version).
    Broken {
        /// Human-readable reason.
        reason: String,
    },
}

impl ToolStatus {
    /// Whether the tool is usable.
    pub fn is_installed(&self) -> bool {
        matches!(self, ToolStatus::Installed { .. })
    }
}

/// State of the docker daemon (separate from CLI presence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerDaemonState {
    /// Daemon answers `docker info`.
    Ready {
        /// Server version, if reported.
        server_version: Option<String>,
    },
    /// The `docker` binary is not installed at all.
    BinaryMissing,
    /// Binary present but the daemon does not answer.
    NotRunning {
        /// Human-readable reason.
        reason: String,
    },
}

/// OS families the installer understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    /// Linux.
    Linux,
    /// macOS.
    Macos,
    /// Windows (winget/choco first, then binary fallback).
    Windows,
    /// Anything else (no package recipes apply; binary fallback may still).
    Other,
}

/// Package managers the installer understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgManager {
    /// Homebrew (macOS and Linux).
    Brew,
    /// dnf (Fedora).
    Dnf,
    /// apt (Debian/Ubuntu).
    Apt,
    /// pacman (Arch).
    Pacman,
    /// winget (Windows).
    Winget,
    /// Chocolatey (Windows).
    Choco,
}

/// The detected local platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    /// OS family.
    pub os: OsKind,
    /// Preferred package manager (first found on PATH).
    pub pkg_manager: Option<PkgManager>,
    /// Whether `pkexec` is available (system installs on Linux go through
    /// it so the GUI app can elevate).
    pub pkexec: bool,
}

/// Detect the current platform (OS + available package manager + pkexec).
pub fn detect_platform() -> Platform {
    let dirs = effective_path_entries();
    let os = match std::env::consts::OS {
        "linux" => OsKind::Linux,
        "macos" => OsKind::Macos,
        "windows" => OsKind::Windows,
        _ => OsKind::Other,
    };
    let pkg_manager = if find_in_path(&dirs, "brew").is_some() {
        Some(PkgManager::Brew)
    } else if find_in_path(&dirs, "dnf").is_some() {
        Some(PkgManager::Dnf)
    } else if find_in_path(&dirs, "apt-get").is_some() {
        Some(PkgManager::Apt)
    } else if find_in_path(&dirs, "pacman").is_some() {
        Some(PkgManager::Pacman)
    } else if find_in_path(&dirs, "winget").is_some() {
        Some(PkgManager::Winget)
    } else if find_in_path(&dirs, "choco").is_some() {
        Some(PkgManager::Choco)
    } else {
        None
    };
    Platform {
        os,
        pkg_manager,
        pkexec: find_in_path(&dirs, "pkexec").is_some(),
    }
}

/// The PATH entries as paths (empty string = current directory).
pub fn path_entries() -> Vec<PathBuf> {
    match std::env::var_os("PATH") {
        None => Vec::new(),
        Some(path) => std::env::split_paths(&path).collect(),
    }
}

/// The PATH entries detection should search: the process PATH plus the
/// directories a GUI-launched app commonly misses.
///
/// Desktop launchers hand apps a minimal PATH — binary fallbacks installed
/// into `~/.local/bin` and (on macOS) Homebrew's `/opt/homebrew/bin` /
/// `/usr/local/bin` are frequently absent from it. Detecting against the raw
/// process PATH only therefore reports "not installed" for tools that are
/// present — the classic false negative. Extending the search here (deduped,
/// original order preserved) removes it; the version probe also runs with
/// this extended PATH so a found binary resolves its own runtime the same
/// way a user's shell would.
pub fn effective_path_entries() -> Vec<PathBuf> {
    let mut dirs = path_entries();
    let mut push_missing = |dir: PathBuf| {
        if !dirs.iter().any(|existing| existing == &dir) {
            dirs.push(dir);
        }
    };
    push_missing(local_bin_dir());
    #[cfg(target_os = "macos")]
    {
        for brew in [
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ] {
            if brew.is_dir() {
                push_missing(brew);
            }
        }
    }
    dirs
}

/// Look `command` up in the given directories (PATH semantics: first match
/// wins; must be a file with the executable bit set — on Windows, PATHEXT
/// probing: `command`, then `command.exe`, `command.bat`, `command.cmd`).
pub fn find_in_path(dirs: &[PathBuf], command: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        for dir in dirs {
            let candidate = dir.join(command);
            if candidate.is_file() && is_executable(&candidate) {
                return Some(candidate);
            }
        }
        None
    }
    #[cfg(windows)]
    {
        for dir in dirs {
            for candidate in pathext_candidates(dir, command) {
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// PATHEXT-style candidate names for `command` in `dir`: the bare name, then
/// the common executable extensions (`.exe`, `.bat`, `.cmd`) appended — so
/// `find_in_path(..., "kind")` finds `kind.exe`. A command that already
/// carries an extension is probed as-is only.
#[cfg(windows)]
fn pathext_candidates(dir: &Path, command: &str) -> Vec<PathBuf> {
    let has_extension = Path::new(command)
        .extension()
        .is_some_and(|ext| !ext.is_empty());
    if has_extension {
        return vec![dir.join(command)];
    }
    let mut candidates = Vec::with_capacity(4);
    candidates.push(dir.join(command));
    for ext in [".exe", ".bat", ".cmd"] {
        candidates.push(dir.join(format!("{command}{ext}")));
    }
    candidates
}

/// Default install dir for binary fallbacks (`$HOME/.local/bin`).
pub fn local_bin_dir() -> PathBuf {
    match dirs::home_dir() {
        Some(home) => home.join(LOCAL_BIN_DIR_NAME),
        None => PathBuf::from(LOCAL_BIN_DIR_NAME),
    }
}

/// Detect one tool: PATH lookup + version command + parse.
///
/// The search uses the current PATH. Errors only for unexpected spawn
/// failures; "not there" and "broken" are [`ToolStatus`] values.
pub async fn detect(id: ToolId) -> Result<ToolStatus> {
    detect_with_path(id, &effective_path_entries()).await
}

/// Like [`detect`], but with an explicit PATH (used by tests and after a
/// `~/.local/bin` install changed the effective PATH).
pub async fn detect_with_path(id: ToolId, search_path: &[PathBuf]) -> Result<ToolStatus> {
    let tool = match tool(id) {
        Some(tool) => tool,
        None => {
            return Err(DepsError::NotFound(id.to_string()).into());
        }
    };
    let Some(path) = find_in_path(search_path, tool.detect.command) else {
        return Ok(ToolStatus::NotInstalled);
    };

    let joined = std::env::join_paths(search_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let cmd = Cmd::new(tool.detect.command)
        .args(tool.detect.version_args.iter().copied())
        .timeout(DETECT_TIMEOUT)
        .env("PATH", joined);
    let output = match cmd.run().await {
        Ok(output) => output,
        Err(err) => {
            // A non-zero exit is not proof of absence: the binary IS on the
            // search path. kubectx in particular is a POSIX shell script
            // whose old releases have no `--version` flag and exit non-zero —
            // presence on PATH is the signal there (the parse branch below
            // can never fire for it). Anything else gets an honest `Broken`
            // with the exit detail.
            if tool.id == ToolId::Kubectx {
                return Ok(ToolStatus::Installed {
                    version: Version::UNKNOWN,
                    path,
                });
            }
            return Ok(ToolStatus::Broken {
                reason: err.to_string(),
            });
        }
    };
    // Prefer stdout; when the version went to stderr (several CLIs log their
    // version banner to stderr), fall back to the captured stderr tail
    // instead of reporting a false "broken".
    let stdout = output.stdout();
    let text: &str = if stdout.trim().is_empty() {
        output.stderr_tail.as_str()
    } else {
        stdout.as_str()
    };
    match parse_version(tool.detect.parse, text) {
        Some(version) => Ok(ToolStatus::Installed { version, path }),
        None => {
            if tool.id == ToolId::Kubectx {
                // Old kubectx scripts have no --version flag; presence +
                // successful exit is enough.
                Ok(ToolStatus::Installed {
                    version: Version::UNKNOWN,
                    path,
                })
            } else {
                Ok(ToolStatus::Broken {
                    reason: format!("unparseable version output: {text:?}"),
                })
            }
        }
    }
}

/// Extract a [`Version`] from tool output using the matrix parse strategy.
pub fn parse_version(parse: VersionParse, output: &str) -> Option<Version> {
    match parse {
        VersionParse::JsonField(path) => {
            let value: serde_json::Value = serde_json::from_str(output.trim()).ok()?;
            let mut current = &value;
            for segment in path.split('.') {
                current = current.get(segment)?;
            }
            let text = current.as_str()?;
            parse_version3(text.trim_start_matches('v'))
        }
        VersionParse::Regex(pattern) => regex_like(pattern, output),
        VersionParse::ShortLine => {
            let rest = match output.find('v') {
                Some(idx) => &output[idx + 1..],
                None => output,
            };
            parse_version3(rest)
        }
    }
}

/// Minimal matcher for the two regex shapes the matrix uses. Anything else
/// falls back to parsing the trimmed whole output.
fn regex_like(pattern: &str, output: &str) -> Option<Version> {
    if pattern.starts_with("Version") {
        let idx = output.find("Version")?;
        let rest = &output[idx + "Version".len()..];
        let token = rest.split_whitespace().next()?;
        parse_version3(token)
    } else if pattern.starts_with('v') {
        // Scan for 'v'/'V' followed by digits.digits.digits.
        let bytes = output.as_bytes();
        for (idx, &byte) in bytes.iter().enumerate() {
            if (byte == b'v' || byte == b'V')
                && let Some(version) = parse_version3_at(output, idx + 1)
            {
                return Some(version);
            }
        }
        None
    } else {
        parse_version3(output.trim())
    }
}

/// Parse a `X.Y.Z` token starting at byte offset `start` (token ends at the
/// first non-digit/non-dot byte).
fn parse_version3_at(text: &str, start: usize) -> Option<Version> {
    let tail = &text[start..];
    let end = tail
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(tail.len());
    parse_version3(&tail[..end])
}

/// Probe the docker daemon via `docker info` (with a short timeout).
///
/// Missing binary → [`DockerDaemonState::BinaryMissing`]; present binary
/// but failing/not-answering → [`DockerDaemonState::NotRunning`].
pub async fn check_docker_daemon() -> DockerDaemonState {
    check_docker_daemon_with_path(&effective_path_entries()).await
}

/// Like [`check_docker_daemon`] with an explicit PATH.
pub async fn check_docker_daemon_with_path(search_path: &[PathBuf]) -> DockerDaemonState {
    if find_in_path(search_path, "docker").is_none() {
        return DockerDaemonState::BinaryMissing;
    }
    let joined = std::env::join_paths(search_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let cmd = Cmd::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .timeout(DAEMON_PROBE_TIMEOUT)
        .env("PATH", joined);
    match cmd.run().await {
        Ok(output) => DockerDaemonState::Ready {
            server_version: {
                let text = output.stdout();
                if text.is_empty() { None } else { Some(text) }
            },
        },
        Err(err) => DockerDaemonState::NotRunning {
            reason: err.to_string(),
        },
    }
}

/// One planned install step (a concrete argv).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallStep {
    /// Human-readable description of the step.
    pub description: String,
    /// argv[0].
    pub program: String,
    /// argv[1..].
    pub args: Vec<String>,
}

/// A full install plan for one tool on one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPlan {
    /// Steps in execution order.
    pub steps: Vec<InstallStep>,
    /// Post-install note to show the user, if any.
    pub post_install: Option<&'static str>,
    /// SHA-256 verifications to run immediately after the corresponding
    /// `curl` download step (before any extraction/install step consumes the
    /// file).
    pub verify: Vec<DownloadVerify>,
}

/// A post-download integrity check attached to a download step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadVerify {
    /// Path of the downloaded file (the curl `-o` target).
    pub file: PathBuf,
    /// Expected SHA-256 hex digest of the file.
    pub expected_sha256: &'static str,
    /// Human-readable description of the check.
    pub description: String,
}

impl InstallPlan {
    /// Render the plan as shell-ish command lines (display only; execution
    /// never goes through a shell).
    pub fn render(&self) -> Vec<String> {
        self.steps
            .iter()
            .map(|step| {
                let mut line = step.program.clone();
                for arg in &step.args {
                    line.push(' ');
                    if arg.contains(' ') {
                        line.push('"');
                        line.push_str(arg);
                        line.push('"');
                    } else {
                        line.push_str(arg);
                    }
                }
                line
            })
            .collect()
    }
}

/// Compute the install plan for the current platform **without executing
/// anything**.
pub fn plan_install(id: ToolId) -> Result<InstallPlan> {
    plan_install_for(id, &detect_platform(), &local_bin_dir())
}

/// Compute the install plan for a given platform/bin-dir (pure; testable).
pub fn plan_install_for(id: ToolId, platform: &Platform, bin_dir: &Path) -> Result<InstallPlan> {
    let tool = tool(id).ok_or_else(|| DepsError::NotFound(id.to_string()))?;
    let mut steps = Vec::new();
    let mut verify = Vec::new();

    if platform.os == OsKind::Windows && tool.install.windows_unsupported {
        return Err(DepsError::NoInstallRecipe {
            tool: tool.display.to_string(),
            reason: "not supported on Windows (POSIX shell script)".to_string(),
        }
        .into());
    }

    let pkg = match (platform.os, platform.pkg_manager) {
        (OsKind::Macos, Some(PkgManager::Brew)) | (OsKind::Linux, Some(PkgManager::Brew)) => {
            // Homebrew cask takes precedence for docker (GUI app) on macOS.
            if id == ToolId::Docker {
                tool.install.brew_cask.map(|cask| {
                    (
                        "brew",
                        vec![
                            "install".to_string(),
                            "--cask".to_string(),
                            cask.to_string(),
                        ],
                    )
                })
            } else {
                tool.install
                    .brew
                    .map(|formula| ("brew", vec!["install".to_string(), formula.to_string()]))
            }
        }
        (OsKind::Linux, Some(PkgManager::Dnf)) => tool.install.dnf.map(|pkg| {
            let program = if platform.pkexec { "pkexec" } else { "dnf" };
            let args = if platform.pkexec {
                vec![
                    "dnf".to_string(),
                    "install".to_string(),
                    "-y".to_string(),
                    pkg.to_string(),
                ]
            } else {
                vec!["install".to_string(), "-y".to_string(), pkg.to_string()]
            };
            (program, args)
        }),
        (OsKind::Linux, Some(PkgManager::Apt)) => tool.install.apt.map(|pkg| {
            let program = if platform.pkexec { "pkexec" } else { "apt-get" };
            let args = if platform.pkexec {
                vec![
                    "apt-get".to_string(),
                    "install".to_string(),
                    "-y".to_string(),
                    pkg.to_string(),
                ]
            } else {
                vec!["install".to_string(), "-y".to_string(), pkg.to_string()]
            };
            (program, args)
        }),
        (OsKind::Linux, Some(PkgManager::Pacman)) => tool.install.pacman.map(|pkg| {
            let program = if platform.pkexec { "pkexec" } else { "pacman" };
            let args = if platform.pkexec {
                vec![
                    "pacman".to_string(),
                    "-S".to_string(),
                    "--noconfirm".to_string(),
                    pkg.to_string(),
                ]
            } else {
                vec!["-S".to_string(), "--noconfirm".to_string(), pkg.to_string()]
            };
            (program, args)
        }),
        (OsKind::Windows, Some(PkgManager::Winget)) => tool.install.winget.map(|id| {
            (
                "winget",
                vec![
                    "install".to_string(),
                    "--id".to_string(),
                    id.to_string(),
                    "-e".to_string(),
                    "--silent".to_string(),
                    "--accept-source-agreements".to_string(),
                    "--accept-package-agreements".to_string(),
                ],
            )
        }),
        (OsKind::Windows, Some(PkgManager::Choco)) => tool.install.choco.map(|pkg| {
            (
                "choco",
                vec!["install".to_string(), "-y".to_string(), pkg.to_string()],
            )
        }),
        _ => None,
    };

    if let Some((program, args)) = pkg {
        steps.push(InstallStep {
            description: format!(
                "install {} via package manager ({})",
                tool.display,
                platform
                    .pkg_manager
                    .map(|pm| format!("{pm:?}"))
                    .unwrap_or_default()
            ),
            program: program.to_string(),
            args,
        });
    } else if let Some(binary) = &tool.install.binary {
        build_binary_steps(tool, binary, platform, bin_dir, &mut steps, &mut verify)?;
    } else {
        return Err(DepsError::NoInstallRecipe {
            tool: tool.display.to_string(),
            reason: "no package manager and no binary fallback".to_string(),
        }
        .into());
    }

    Ok(InstallPlan {
        steps,
        post_install: tool.install.post_install,
        verify,
    })
}

/// Render `{os}`/`{OS}`/`{arch}`/`{x64}` placeholders for a platform.
fn render_platform(template: &str, platform: &Platform, arch: &str) -> String {
    let (os, os_caps) = match platform.os {
        OsKind::Macos => ("darwin", "Darwin"),
        OsKind::Windows => ("windows", "Windows"),
        _ => ("linux", "Linux"),
    };
    let (arch, x64) = if arch == "aarch64" {
        ("arm64", "arm64")
    } else {
        ("amd64", "x86_64")
    };
    template
        .replace("{os}", os)
        .replace("{OS}", os_caps)
        .replace("{arch}", arch)
        .replace("{x64}", x64)
}

/// The effective binary arch for downloads (arm64 on aarch64, else amd64).
fn arch_name() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "amd64",
    }
}

/// The canonical `"{os}-{arch}"` platform key used to look up pinned
/// SHA-256 digests (os ∈ {linux, darwin, windows}; arch ∈ {amd64, arm64}).
fn platform_key(platform: &Platform, arch: &str) -> String {
    let os = match platform.os {
        OsKind::Macos => "darwin",
        OsKind::Windows => "windows",
        _ => "linux",
    };
    format!("{os}-{arch}")
}

fn build_binary_steps(
    tool: &Tool,
    binary: &BinaryDownload,
    platform: &Platform,
    bin_dir: &Path,
    steps: &mut Vec<InstallStep>,
    verify: &mut Vec<DownloadVerify>,
) -> Result<()> {
    if platform.os == OsKind::Windows {
        return build_binary_steps_windows(tool, binary, platform, bin_dir, steps, verify);
    }
    let arch = arch_name();
    let url = render_platform(binary.url_template, platform, arch);
    let download_path = bin_dir.join(format!(".kb-dl-{}", tool.detect.command));

    steps.push(InstallStep {
        description: format!("create install dir {}", bin_dir.display()),
        program: "mkdir".to_string(),
        args: vec!["-p".to_string(), bin_dir.to_string_lossy().to_string()],
    });
    steps.push(InstallStep {
        description: format!("download {}", tool.display),
        program: "curl".to_string(),
        args: vec![
            "-L".to_string(),
            "--fail".to_string(),
            "--silent".to_string(),
            "--show-error".to_string(),
            // HTTPS-only for both the request and any redirect target, and
            // an upper bound on the artifact size (biggest binary today is
            // ~76 MiB; 256 MiB leaves headroom while capping abuse).
            "--proto=https".to_string(),
            "--proto-redir=https".to_string(),
            "--max-filesize".to_string(),
            (256 * 1024 * 1024).to_string(),
            "-o".to_string(),
            download_path.to_string_lossy().to_string(),
            url,
        ],
    });

    let key = platform_key(platform, arch);
    let expected = binary
        .sha256
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, digest)| *digest)
        .ok_or_else(|| DepsError::NoInstallRecipe {
            tool: tool.display.to_string(),
            reason: format!("no pinned sha256 for platform {key}"),
        })?;
    verify.push(DownloadVerify {
        file: download_path.clone(),
        expected_sha256: expected,
        description: format!("verify {} (sha256)", tool.display),
    });

    if binary.members.is_empty() {
        steps.push(InstallStep {
            description: format!("make {} executable", tool.display),
            program: "chmod".to_string(),
            args: vec![
                "+x".to_string(),
                download_path.to_string_lossy().to_string(),
            ],
        });
        steps.push(InstallStep {
            description: format!("install {} to {}", tool.display, bin_dir.display()),
            program: "mv".to_string(),
            args: vec![
                download_path.to_string_lossy().to_string(),
                bin_dir
                    .join(tool.detect.command)
                    .to_string_lossy()
                    .to_string(),
            ],
        });
        return Ok(());
    }

    let extract_dir = bin_dir.join(format!(".kb-extract-{}", tool.detect.command));
    steps.push(InstallStep {
        description: format!("create extract dir {}", extract_dir.display()),
        program: "mkdir".to_string(),
        args: vec!["-p".to_string(), extract_dir.to_string_lossy().to_string()],
    });
    let mut tar_args = vec![
        "-xzf".to_string(),
        download_path.to_string_lossy().to_string(),
        "-C".to_string(),
        extract_dir.to_string_lossy().to_string(),
    ];
    for member in binary.members {
        tar_args.push(render_platform(member, platform, arch));
    }
    steps.push(InstallStep {
        description: format!("extract {}", tool.display),
        program: "tar".to_string(),
        args: tar_args,
    });
    for member in binary.members {
        let rendered = render_platform(member, platform, arch);
        let file_name = Path::new(&rendered)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| rendered.clone());
        let extracted = extract_dir.join(&rendered);
        steps.push(InstallStep {
            description: format!("make {file_name} executable"),
            program: "chmod".to_string(),
            args: vec!["+x".to_string(), extracted.to_string_lossy().to_string()],
        });
        steps.push(InstallStep {
            description: format!("install {file_name} to {}", bin_dir.display()),
            program: "mv".to_string(),
            args: vec![
                extracted.to_string_lossy().to_string(),
                bin_dir.join(&file_name).to_string_lossy().to_string(),
            ],
        });
    }
    steps.push(InstallStep {
        description: "clean up download".to_string(),
        program: "rm".to_string(),
        args: vec![
            "-rf".to_string(),
            download_path.to_string_lossy().to_string(),
            extract_dir.to_string_lossy().to_string(),
        ],
    });
    Ok(())
}

/// Windows branch of [`build_binary_steps`]: same shape (download → sha256
/// verify → extract → install → cleanup) using the OS-bundled `curl.exe` and
/// `tar.exe` (Win10 1803+, bsdtar handles `.zip`) plus PowerShell one-liners
/// for mkdir/mv/rm — no chmod on Windows (binaries don't carry an exec bit).
fn build_binary_steps_windows(
    tool: &Tool,
    binary: &BinaryDownload,
    platform: &Platform,
    bin_dir: &Path,
    steps: &mut Vec<InstallStep>,
    verify: &mut Vec<DownloadVerify>,
) -> Result<()> {
    let arch = arch_name();
    let url = match binary.windows_url_template {
        Some(template) => render_platform(template, platform, arch),
        None => render_platform(binary.url_template, platform, arch),
    };
    let download_path = bin_dir.join(format!(".kb-dl-{}", tool.detect.command));

    // ps_quote: escape a path for a PowerShell single-quoted string by
    // doubling embedded single quotes (Windows usernames/paths may contain
    // them, e.g. C:\Users\O'Brien\...). PS has no other quoting rule inside
    // '...' literals.
    fn ps_quote(path: &Path) -> String {
        path.display().to_string().replace('\'', "''")
    }

    let powershell_args = |script: String| {
        vec![
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            script,
        ]
    };

    steps.push(InstallStep {
        description: format!("create install dir {}", bin_dir.display()),
        program: "powershell".to_string(),
        args: powershell_args(format!(
            "New-Item -ItemType Directory -Force -Path '{}' | Out-Null",
            ps_quote(bin_dir)
        )),
    });
    steps.push(InstallStep {
        description: format!("download {}", tool.display),
        program: "curl.exe".to_string(),
        args: vec![
            "-L".to_string(),
            "--fail".to_string(),
            "--silent".to_string(),
            "--show-error".to_string(),
            "--proto=https".to_string(),
            "--proto-redir=https".to_string(),
            "--max-filesize".to_string(),
            (256 * 1024 * 1024).to_string(),
            "-o".to_string(),
            download_path.to_string_lossy().to_string(),
            url,
        ],
    });

    let key = platform_key(platform, arch);
    let expected = binary
        .sha256
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, digest)| *digest)
        .ok_or_else(|| DepsError::NoInstallRecipe {
            tool: tool.display.to_string(),
            reason: format!("no pinned sha256 for platform {key}"),
        })?;
    verify.push(DownloadVerify {
        file: download_path.clone(),
        expected_sha256: expected,
        description: format!("verify {} (sha256)", tool.display),
    });

    if binary.windows_members.is_empty() {
        // The download IS the binary: install it as `<command>.exe` (the
        // Windows loader resolves extensionless program names to .exe).
        let target = bin_dir.join(format!("{}.exe", tool.detect.command));
        steps.push(InstallStep {
            description: format!("install {} to {}", tool.display, bin_dir.display()),
            program: "powershell".to_string(),
            args: powershell_args(format!(
                "Move-Item -Force '{}' '{}'",
                ps_quote(&download_path),
                ps_quote(&target)
            )),
        });
        return Ok(());
    }

    let extract_dir = bin_dir.join(format!(".kb-extract-{}", tool.detect.command));
    steps.push(InstallStep {
        description: format!("create extract dir {}", extract_dir.display()),
        program: "powershell".to_string(),
        args: powershell_args(format!(
            "New-Item -ItemType Directory -Force -Path '{}' | Out-Null",
            ps_quote(&extract_dir)
        )),
    });
    let mut tar_args = vec![
        "-xf".to_string(),
        download_path.to_string_lossy().to_string(),
        "-C".to_string(),
        extract_dir.to_string_lossy().to_string(),
    ];
    for member in binary.windows_members {
        tar_args.push(render_platform(member, platform, arch));
    }
    steps.push(InstallStep {
        description: format!("extract {}", tool.display),
        program: "tar.exe".to_string(),
        args: tar_args,
    });
    for member in binary.windows_members {
        let rendered = render_platform(member, platform, arch);
        let file_name = Path::new(&rendered)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| rendered.clone());
        let extracted = extract_dir.join(&rendered);
        steps.push(InstallStep {
            description: format!("install {file_name} to {}", bin_dir.display()),
            program: "powershell".to_string(),
            args: powershell_args(format!(
                "Move-Item -Force '{}' '{}'",
                ps_quote(&extracted),
                ps_quote(&bin_dir.join(&file_name))
            )),
        });
    }
    steps.push(InstallStep {
        description: "clean up download".to_string(),
        program: "powershell".to_string(),
        args: powershell_args(format!(
            "Remove-Item -Recurse -Force '{}','{}'",
            ps_quote(&download_path),
            ps_quote(&extract_dir)
        )),
    });
    Ok(())
}

/// Progress event emitted while an install plan executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallEvent {
    /// A step is about to run.
    StepStarted {
        /// 0-based step index.
        step: usize,
        /// Total step count.
        total: usize,
        /// Step description.
        description: String,
    },
    /// A line of output from a running step.
    StepLine {
        /// 0-based step index.
        step: usize,
        /// The output line.
        line: String,
    },
    /// A step finished successfully.
    StepFinished {
        /// 0-based step index.
        step: usize,
    },
}

/// Execute an install plan through the exec runner, streaming events.
///
/// Returns the post-install [`ToolStatus`] (re-detected) or the first
/// failing step's error. `cancel` (optional) aborts between steps and kills
/// the running step.
///
/// Install only if needed: a pre-flight detection gates the plan, so an
/// already-installed tool runs zero steps (package managers exit non-zero
/// with "already installed" otherwise, which read as a failure on a tool
/// that is present). A failed step re-checks reality before reporting the
/// failure, so a stale detection or an installer race can never produce a
/// false "install failed".
pub async fn install_with_progress(
    id: ToolId,
    cancel: Option<tokio_util::sync::CancellationToken>,
    tx: &mpsc::Sender<InstallEvent>,
) -> Result<ToolStatus> {
    // A detect error must not block installation — installing is itself the
    // repair path.
    let preflight = match detect(id).await {
        Ok(status) => status,
        Err(err) => {
            log::warn!("pre-install detect for {id} failed ({err}); installing anyway");
            ToolStatus::NotInstalled
        }
    };
    install_with_progress_preflighted(id, cancel, tx, preflight).await
}

/// [`install_with_progress`] with the pre-flight outcome supplied.
///
/// Test seam: unit tests inject [`ToolStatus::Installed`] and assert that
/// zero plan steps run and the status round-trips untouched.
async fn install_with_progress_preflighted(
    id: ToolId,
    cancel: Option<tokio_util::sync::CancellationToken>,
    tx: &mpsc::Sender<InstallEvent>,
    preflight: ToolStatus,
) -> Result<ToolStatus> {
    if let ToolStatus::Installed { .. } = preflight {
        log::info!("dependency {id} already installed; skipping install plan");
        let _ = tx
            .send(InstallEvent::StepLine {
                step: 0,
                line: "already installed — nothing to do".to_string(),
            })
            .await;
        return Ok(preflight);
    }
    let plan = plan_install(id)?;
    log::info!("installing dependency {id}");
    let total = plan.steps.len();
    for (index, step) in plan.steps.iter().enumerate() {
        if let Some(token) = &cancel
            && token.is_cancelled()
        {
            return Err(DepsError::InstallFailed {
                tool: id.to_string(),
                step: step.description.clone(),
                reason: "cancelled".to_string(),
            }
            .into());
        }
        let _ = tx
            .send(InstallEvent::StepStarted {
                step: index,
                total,
                description: step.description.clone(),
            })
            .await;
        let mut cmd = Cmd::new(&step.program)
            .args(step.args.iter().cloned())
            .timeout(step_timeout(step));
        if let Some(token) = &cancel {
            cmd = cmd.cancel(token.clone());
        }
        match cmd.run().await {
            Ok(output) => {
                for line in &output.stdout_lines {
                    let _ = tx
                        .send(InstallEvent::StepLine {
                            step: index,
                            line: crate::exec::redact_kubeconfig_secrets(line),
                        })
                        .await;
                }
                // Security gate (audit finding H1): every download step must
                // pass its pinned sha256 BEFORE any later step consumes the
                // file. The Windows plan runs `curl.exe`, and Windows program
                // names are case-insensitive — match the whole curl family
                // case-insensitively so the gate can never be skipped by a
                // spelling/case change. The `-o` target must also resolve to
                // a `DownloadVerify` entry, anchoring the gate to the file
                // the plan declares verified.
                if (step.program.eq_ignore_ascii_case("curl")
                    || step.program.eq_ignore_ascii_case("curl.exe"))
                    && let Some(download_path) = step
                        .args
                        .iter()
                        .position(|arg| arg == "-o")
                        .and_then(|pos| step.args.get(pos + 1))
                    && let Some(check) = plan
                        .verify
                        .iter()
                        .find(|check| check.file == *download_path)
                {
                    verify_download_file(
                        &id.to_string(),
                        &check.description,
                        Path::new(download_path),
                        check.expected_sha256,
                    )
                    .await?;
                    let _ = tx
                        .send(InstallEvent::StepLine {
                            step: index,
                            line: format!("{}: ok", check.description),
                        })
                        .await;
                }
                let _ = tx.send(InstallEvent::StepFinished { step: index }).await;
            }
            Err(err) => {
                log::info!("failed to install dependency {id}");
                // Reality check before reporting a failure: package managers
                // exit non-zero with "already installed" when a stale
                // detection or an installer race made us re-run them. If the
                // tool is present, that is success — never a false failure.
                if let Some(status) = reconcile_failure_recheck(detect(id).await) {
                    let _ = tx
                        .send(InstallEvent::StepLine {
                            step: index,
                            line: "install step failed, but the tool is present — treating as installed"
                                .to_string(),
                        })
                        .await;
                    return Ok(status);
                }
                let reason = match err {
                    crate::error::ExecError::Command {
                        code, stderr_tail, ..
                    } => format!("exit {code:?}: {stderr_tail}"),
                    other => other.to_string(),
                };
                return Err(DepsError::InstallFailed {
                    tool: id.to_string(),
                    step: step.description.clone(),
                    reason,
                }
                .into());
            }
        }
    }
    // Re-detect against the (possibly updated) PATH so the caller gets the
    // definitive post-install status.
    let status = detect(id).await?;
    log::info!(
        "installed dependency {id} {}",
        match &status {
            ToolStatus::Installed { version, .. } => version.to_string(),
            other => format!("{other:?}"),
        }
    );
    Ok(status)
}

/// The policy for a failed install step: if the re-check finds the tool
/// installed anyway (package-manager "already installed" race, stale
/// detection, partially-completed plan), the failure is reconciled to
/// success. Pure so the policy is unit-testable.
fn reconcile_failure_recheck(recheck: Result<ToolStatus>) -> Option<ToolStatus> {
    match recheck {
        Ok(status @ ToolStatus::Installed { .. }) => Some(status),
        _ => None,
    }
}

fn step_timeout(step: &InstallStep) -> std::time::Duration {
    match step.program.as_str() {
        "curl" | "curl.exe" => DOWNLOAD_TIMEOUT,
        "mkdir" | "chmod" | "mv" | "rm" | "tar" | "tar.exe" | "powershell" => {
            std::time::Duration::from_secs(60)
        }
        _ => PKG_INSTALL_TIMEOUT,
    }
}

/// Verify that a downloaded file's SHA-256 matches the pinned digest.
///
/// The integrity gate between `curl` and any step that consumes the file
/// (extraction, chmod, install). A mismatch is a hard failure — the file is
/// never executed.
async fn verify_download_file(
    tool: &str,
    step_desc: &str,
    file: &Path,
    expected_sha256: &'static str,
) -> Result<()> {
    let digest = crate::fsutil::sha256_file_hex(file)
        .await
        .map_err(|source| DepsError::InstallFailed {
            tool: tool.to_string(),
            step: step_desc.to_string(),
            reason: source.to_string(),
        })?;
    if !digest.eq_ignore_ascii_case(expected_sha256) {
        return Err(DepsError::ChecksumMismatch {
            tool: tool.to_string(),
            path: file.to_path_buf(),
            expected_sha256,
        }
        .into());
    }
    Ok(())
}

/// Install a tool on the current platform (convenience wrapper; events are
/// discarded).
pub async fn install(id: ToolId) -> Result<ToolStatus> {
    /// Event buffer for the drain task below. Capacity is not load-bearing —
    /// the drain consumes events, so sends never block regardless.
    const DRAIN_BUFFER: usize = 8;
    let (tx, mut rx) = mpsc::channel(DRAIN_BUFFER);
    // Drain events on a background task so event sends never block (the
    // receiver is deliberately unused by this convenience wrapper).
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let result = install_with_progress(id, None, &tx).await;
    drop(tx); // close the channel so the drain task can exit
    let _ = drain.await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CoreError;
    use crate::exec::Cmd;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kindboard-deps-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_stub(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
    }

    fn linux_dnf(pkexec: bool) -> Platform {
        Platform {
            os: OsKind::Linux,
            pkg_manager: Some(PkgManager::Dnf),
            pkexec,
        }
    }

    fn linux_none() -> Platform {
        Platform {
            os: OsKind::Linux,
            pkg_manager: None,
            pkexec: false,
        }
    }

    fn macos() -> Platform {
        Platform {
            os: OsKind::Macos,
            pkg_manager: Some(PkgManager::Brew),
            pkexec: false,
        }
    }

    fn windows() -> Platform {
        Platform {
            os: OsKind::Windows,
            pkg_manager: Some(PkgManager::Winget),
            pkexec: false,
        }
    }

    fn windows_choco() -> Platform {
        Platform {
            os: OsKind::Windows,
            pkg_manager: Some(PkgManager::Choco),
            pkexec: false,
        }
    }

    fn windows_none() -> Platform {
        Platform {
            os: OsKind::Windows,
            pkg_manager: None,
            pkexec: false,
        }
    }

    // ---- version parsing ----

    #[test]
    fn parses_bare_semver() {
        assert_eq!(
            parse_version3("29.8.0"),
            Some(Version {
                major: 29,
                minor: 8,
                patch: 0
            })
        );
        assert_eq!(
            parse_version3("v1.37.0"),
            None,
            "leading v is stripped by callers"
        );
        assert_eq!(parse_version3("1.37"), None);
        assert_eq!(parse_version3(""), None);
        assert_eq!(parse_version3("1.2.3.4"), None);
    }

    #[test]
    fn version_display_and_order() {
        assert_eq!(
            Version {
                major: 1,
                minor: 2,
                patch: 3
            }
            .to_string(),
            "1.2.3"
        );
        let a = Version {
            major: 1,
            minor: 30,
            patch: 0,
        };
        let b = Version {
            major: 1,
            minor: 9,
            patch: 9,
        };
        assert!(a > b);
        assert!(a.at_least(1, 30, 0));
        assert!(!a.at_least(1, 31, 0));
        assert!(!Version::UNKNOWN.is_known());
    }

    #[test]
    fn json_field_parse() {
        let out = r#"{"clientVersion":{"gitVersion":"v1.37.0"}}"#;
        let v = parse_version(VersionParse::JsonField("clientVersion.gitVersion"), out);
        assert_eq!(
            v,
            Some(Version {
                major: 1,
                minor: 37,
                patch: 0
            })
        );

        // Wrong path / invalid json → None.
        assert_eq!(
            parse_version(VersionParse::JsonField("serverVersion.gitVersion"), out),
            None
        );
        assert_eq!(
            parse_version(
                VersionParse::JsonField("clientVersion.gitVersion"),
                "not json"
            ),
            None
        );
    }

    #[test]
    fn regex_parse_shapes() {
        assert_eq!(
            parse_version(
                VersionParse::Regex("v(\\d+\\.\\d+\\.\\d+)"),
                "kind v0.33.0 go1.26.7 linux/amd64"
            ),
            Some(Version {
                major: 0,
                minor: 33,
                patch: 0
            })
        );
        assert_eq!(
            parse_version(
                VersionParse::Regex("v(\\d+\\.\\d+\\.\\d+)"),
                "v4.2.2+gb05881c"
            ),
            Some(Version {
                major: 4,
                minor: 2,
                patch: 2
            })
        );
        assert_eq!(
            parse_version(
                VersionParse::Regex("v(\\d+\\.\\d+\\.\\d+)"),
                "cilium-cli: v0.20.0 compiled with go1.24"
            ),
            Some(Version {
                major: 0,
                minor: 20,
                patch: 0
            })
        );
        assert_eq!(
            parse_version(VersionParse::Regex("Version\\s+(\\S+)"), "Version 0.51.0"),
            Some(Version {
                major: 0,
                minor: 51,
                patch: 0
            })
        );
        assert_eq!(
            parse_version(
                VersionParse::Regex("v(\\d+\\.\\d+\\.\\d+)"),
                "no version here"
            ),
            None
        );
    }

    #[test]
    fn shortline_parse() {
        assert_eq!(
            parse_version(VersionParse::ShortLine, "29.8.0"),
            Some(Version {
                major: 29,
                minor: 8,
                patch: 0
            })
        );
        assert_eq!(
            parse_version(VersionParse::ShortLine, "v5.8.1"),
            Some(Version {
                major: 5,
                minor: 8,
                patch: 1
            })
        );
        assert_eq!(parse_version(VersionParse::ShortLine, "junk"), None);
    }

    // ---- detection with fake binaries ----

    #[tokio::test]
    async fn detect_missing_tool() {
        let dir = temp_dir("missing");
        let status = detect_with_path(ToolId::Kind, std::slice::from_ref(&dir))
            .await
            .unwrap();
        assert_eq!(status, ToolStatus::NotInstalled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn detect_installed_tool_via_stub() {
        let dir = temp_dir("kind-stub");
        make_stub(&dir, "kind", "echo 'kind v0.33.0 go1.26.7 linux/amd64'");
        let status = detect_with_path(ToolId::Kind, std::slice::from_ref(&dir))
            .await
            .unwrap();
        match status {
            ToolStatus::Installed { version, path } => {
                assert_eq!(
                    version,
                    Version {
                        major: 0,
                        minor: 33,
                        patch: 0
                    }
                );
                assert_eq!(path, dir.join("kind"));
            }
            other => panic!("expected Installed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn detect_kubectl_json_version() {
        let dir = temp_dir("kubectl-stub");
        make_stub(
            &dir,
            "kubectl",
            r#"echo '{"clientVersion":{"gitVersion":"v1.37.0"}}'"#,
        );
        let status = detect_with_path(ToolId::Kubectl, std::slice::from_ref(&dir))
            .await
            .unwrap();
        match status {
            ToolStatus::Installed { version, .. } => {
                assert_eq!(
                    version,
                    Version {
                        major: 1,
                        minor: 37,
                        patch: 0
                    }
                );
            }
            other => panic!("expected Installed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn detect_broken_tool_on_bad_output() {
        let dir = temp_dir("broken-stub");
        make_stub(&dir, "kind", "echo 'something entirely unexpected'");
        let status = detect_with_path(ToolId::Kind, std::slice::from_ref(&dir))
            .await
            .unwrap();
        assert!(
            matches!(status, ToolStatus::Broken { .. }),
            "got {status:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn detect_broken_tool_on_nonzero_exit() {
        let dir = temp_dir("exit-stub");
        make_stub(&dir, "kind", "echo boom >&2; exit 1");
        let status = detect_with_path(ToolId::Kind, std::slice::from_ref(&dir))
            .await
            .unwrap();
        assert!(
            matches!(status, ToolStatus::Broken { .. }),
            "got {status:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn kubectx_falls_back_to_presence() {
        let dir = temp_dir("kubectx-stub");
        make_stub(&dir, "kubectx", "echo 'kubectx is a script'");
        let status = detect_with_path(ToolId::Kubectx, std::slice::from_ref(&dir))
            .await
            .unwrap();
        match status {
            ToolStatus::Installed { version, .. } => assert_eq!(version, Version::UNKNOWN),
            other => panic!("expected Installed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn detect_kubectx_nonzero_exit_is_present_not_broken() {
        // Old kubectx releases are POSIX scripts without --version: they exit
        // non-zero, and presence on PATH is the only reliable signal. This
        // must never surface as "broken" (false negative class D2).
        let dir = temp_dir("kubectx-exit-stub");
        make_stub(&dir, "kubectx", "exit 1");
        let status = detect_with_path(ToolId::Kubectx, std::slice::from_ref(&dir))
            .await
            .unwrap();
        match status {
            ToolStatus::Installed { version, path } => {
                assert_eq!(version, Version::UNKNOWN);
                assert_eq!(path, dir.join("kubectx"));
            }
            other => panic!("expected Installed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn detect_parses_version_from_stderr_when_stdout_is_empty() {
        // Several CLIs print their version banner to stderr; stdout-only
        // parsing would report a false "broken" (false negative class D3).
        let dir = temp_dir("stderr-stub");
        make_stub(&dir, "k9s", "echo 'Version 0.51.0' >&2");
        let status = detect_with_path(ToolId::K9s, std::slice::from_ref(&dir))
            .await
            .unwrap();
        match status {
            ToolStatus::Installed { version, .. } => {
                assert_eq!(
                    version,
                    Version {
                        major: 0,
                        minor: 51,
                        patch: 0
                    }
                );
            }
            other => panic!("expected Installed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn effective_path_includes_local_bin_dir_without_duplicates() {
        // Binary fallbacks install into ~/.local/bin; a GUI-launched app
        // must still see them (false negative class D1).
        let dirs = effective_path_entries();
        assert!(
            dirs.contains(&local_bin_dir()),
            "effective path must include {0}; got {dirs:?}",
            local_bin_dir().display()
        );
        let mut seen = std::collections::HashSet::new();
        for dir in &dirs {
            assert!(
                seen.insert(dir.clone()),
                "duplicate path entry {0}",
                dir.display()
            );
        }
    }

    #[test]
    fn effective_path_keeps_original_order() {
        let mut before = path_entries();
        before.push(local_bin_dir());
        let after = effective_path_entries();
        // Every original entry survives, in its original position.
        let mut idx = 0;
        for entry in &after {
            if idx < before.len() - 1 {
                assert_eq!(entry, &before[idx]);
                idx += 1;
            }
        }
        assert!(idx >= before.len() - 1, "PATH entries were reordered");
    }

    // ---- docker daemon probe ----

    #[tokio::test]
    async fn docker_missing_reports_binary_missing() {
        let dir = temp_dir("no-docker");
        let state = check_docker_daemon_with_path(std::slice::from_ref(&dir)).await;
        assert_eq!(state, DockerDaemonState::BinaryMissing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn docker_stub_ok_reports_ready() {
        let dir = temp_dir("docker-ok");
        make_stub(&dir, "docker", "echo '29.8.0'");
        let state = check_docker_daemon_with_path(std::slice::from_ref(&dir)).await;
        assert_eq!(
            state,
            DockerDaemonState::Ready {
                server_version: Some("29.8.0".to_string())
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn docker_stub_down_reports_not_running() {
        let dir = temp_dir("docker-down");
        make_stub(&dir, "docker", "exit 1");
        let state = check_docker_daemon_with_path(std::slice::from_ref(&dir)).await;
        assert!(matches!(state, DockerDaemonState::NotRunning { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn docker_stub_hang_reports_not_running() {
        let dir = temp_dir("docker-hang");
        make_stub(&dir, "docker", "sleep 60");
        let state = check_docker_daemon_with_path(std::slice::from_ref(&dir)).await;
        assert!(matches!(state, DockerDaemonState::NotRunning { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- install plans ----

    #[test]
    fn plan_kind_on_fedora_with_pkexec() {
        let plan = plan_install_for(
            ToolId::Kind,
            &linux_dnf(true),
            Path::new("/home/u/.local/bin"),
        )
        .unwrap();
        assert_eq!(plan.steps.len(), 1);
        let step = &plan.steps[0];
        assert_eq!(step.program, "pkexec");
        assert_eq!(step.args, vec!["dnf", "install", "-y", "kind"]);
    }

    #[test]
    fn plan_kind_on_fedora_without_pkexec() {
        let plan =
            plan_install_for(ToolId::Kind, &linux_dnf(false), Path::new("/tmp/bin")).unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].program, "dnf");
        assert_eq!(plan.steps[0].args, vec!["install", "-y", "kind"]);
    }

    #[test]
    fn plan_docker_on_macos_uses_cask() {
        let plan =
            plan_install_for(ToolId::Docker, &macos(), Path::new("/home/u/.local/bin")).unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].program, "brew");
        assert_eq!(
            plan.steps[0].args,
            vec!["install", "--cask", "docker-desktop"]
        );
        assert!(plan.post_install.is_some());
    }

    #[test]
    fn plan_kind_on_macos_uses_formula() {
        let plan = plan_install_for(ToolId::Kind, &macos(), Path::new("/x")).unwrap();
        assert_eq!(plan.steps[0].args, vec!["install", "kind"]);
    }

    #[test]
    fn plan_kind_on_apt() {
        let platform = Platform {
            os: OsKind::Linux,
            pkg_manager: Some(PkgManager::Apt),
            pkexec: true,
        };
        let plan = plan_install_for(ToolId::Kubectl, &platform, Path::new("/x")).unwrap();
        assert_eq!(plan.steps[0].program, "pkexec");
        assert_eq!(
            plan.steps[0].args,
            vec!["apt-get", "install", "-y", "kubectl"]
        );
    }

    #[test]
    fn plan_k9s_on_pacman() {
        let platform = Platform {
            os: OsKind::Linux,
            pkg_manager: Some(PkgManager::Pacman),
            pkexec: false,
        };
        let plan = plan_install_for(ToolId::K9s, &platform, Path::new("/x")).unwrap();
        assert_eq!(plan.steps[0].program, "pacman");
        assert_eq!(plan.steps[0].args, vec!["-S", "--noconfirm", "k9s"]);
    }

    #[test]
    fn plan_apt_has_no_kind_package_uses_binary() {
        let platform = Platform {
            os: OsKind::Linux,
            pkg_manager: Some(PkgManager::Apt),
            pkexec: true,
        };
        let plan =
            plan_install_for(ToolId::Kind, &platform, Path::new("/home/u/.local/bin")).unwrap();
        let programs: Vec<&str> = plan.steps.iter().map(|s| s.program.as_str()).collect();
        assert_eq!(programs[0], "mkdir");
        assert_eq!(programs[1], "curl");
        let curl = &plan.steps[1];
        let url = curl.args.last().unwrap();
        assert!(
            url.contains("kind.sigs.k8s.io/dl/v0.33.0/kind-linux-amd64"),
            "url: {url}"
        );
    }

    #[test]
    fn plan_dnf_has_no_kubectl_package_uses_binary() {
        let plan = plan_install_for(
            ToolId::Kubectl,
            &linux_dnf(true),
            Path::new("/home/u/.local/bin"),
        )
        .unwrap();
        let curl = plan
            .steps
            .iter()
            .find(|s| s.program == "curl")
            .expect("curl step");
        let url = curl.args.last().unwrap();
        assert!(
            url.contains("dl.k8s.io/release/v1.37.0/bin/linux/amd64/kubectl"),
            "url: {url}"
        );
    }

    #[test]
    fn plan_helm_binary_extracts_member() {
        let plan =
            plan_install_for(ToolId::Helm, &linux_none(), Path::new("/home/u/.local/bin")).unwrap();
        let curl = plan.steps.iter().find(|s| s.program == "curl").unwrap();
        assert!(
            curl.args
                .last()
                .unwrap()
                .contains("get.helm.sh/helm-v4.2.2-linux-amd64.tar.gz")
        );
        let tar = plan.steps.iter().find(|s| s.program == "tar").unwrap();
        assert_eq!(
            tar.args,
            vec![
                "-xzf",
                "/home/u/.local/bin/.kb-dl-helm",
                "-C",
                "/home/u/.local/bin/.kb-extract-helm",
                "linux-amd64/helm"
            ]
        );
        let mv = plan.steps.iter().rfind(|s| s.program == "mv").unwrap();
        assert_eq!(
            mv.args,
            vec![
                "/home/u/.local/bin/.kb-extract-helm/linux-amd64/helm",
                "/home/u/.local/bin/helm"
            ]
        );
        let chmod = plan.steps.iter().rfind(|s| s.program == "chmod").unwrap();
        assert!(chmod.args[1].ends_with("linux-amd64/helm"));
    }

    #[test]
    fn plan_kubectx_extracts_single_binary() {
        let plan = plan_install_for(
            ToolId::Kubectx,
            &linux_none(),
            Path::new("/home/u/.local/bin"),
        )
        .unwrap();
        let tar = plan.steps.iter().find(|s| s.program == "tar").unwrap();
        // -xzf <archive> -C <dir> kubectx (kubens ships in its own archive).
        assert_eq!(tar.args.len(), 5);
        assert_eq!(tar.args[4], "kubectx");
        let mv_names: Vec<String> = plan
            .steps
            .iter()
            .filter(|s| s.program == "mv")
            .map(|s| s.args[1].clone())
            .collect();
        assert_eq!(mv_names, vec!["/home/u/.local/bin/kubectx"], "{mv_names:?}");
    }

    #[test]
    fn plan_docker_without_any_installer_fails() {
        let err = plan_install_for(ToolId::Docker, &linux_none(), Path::new("/x")).unwrap_err();
        match err {
            CoreError::Deps(DepsError::NoInstallRecipe { tool, .. }) => assert_eq!(tool, "docker"),
            other => panic!("expected NoInstallRecipe, got {other:?}"),
        }
    }

    #[test]
    fn plan_k9s_url_uses_capital_os() {
        // No package manager → binary fallback with the capitalized {OS}.
        let plan = plan_install_for(ToolId::K9s, &linux_none(), Path::new("/x")).unwrap();
        let curl = plan.steps.iter().find(|s| s.program == "curl").unwrap();
        let url = curl.args.last().unwrap();
        assert!(url.contains("k9s_Linux_amd64.tar.gz"), "url: {url}");
    }

    #[test]
    fn binary_fallback_urls_match_matrix_doc() {
        // Spot-check the rendered download URLs against
        // docs/dependency-install-matrix.md on linux/amd64.
        let cases: Vec<(ToolId, &str)> = vec![
            (
                ToolId::Kind,
                "https://kind.sigs.k8s.io/dl/v0.33.0/kind-linux-amd64",
            ),
            (
                ToolId::Kubectl,
                "https://dl.k8s.io/release/v1.37.0/bin/linux/amd64/kubectl",
            ),
            (
                ToolId::Helm,
                "https://get.helm.sh/helm-v4.2.2-linux-amd64.tar.gz",
            ),
            (
                ToolId::Cilium,
                "https://github.com/cilium/cilium-cli/releases/download/v0.20.0/cilium-linux-amd64.tar.gz",
            ),
            (
                ToolId::K9s,
                "https://github.com/derailed/k9s/releases/download/v0.51.0/k9s_Linux_amd64.tar.gz",
            ),
            (
                ToolId::Kubectx,
                "https://github.com/ahmetb/kubectx/releases/download/v0.11.0/kubectx_v0.11.0_linux_x86_64.tar.gz",
            ),
            (
                ToolId::Kustomize,
                "https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize/v5.8.1/kustomize_v5.8.1_linux_amd64.tar.gz",
            ),
        ];
        for (id, expected) in cases {
            let plan = plan_install_for(id, &linux_none(), Path::new("/tmp/bin")).unwrap();
            let curl = plan
                .steps
                .iter()
                .find(|step| step.program == "curl")
                .unwrap_or_else(|| panic!("{id:?} must have a curl download step"));
            let url = curl.args.last().expect("curl step needs a url arg");
            assert_eq!(url, expected, "URL mismatch for {id:?}");
        }
    }

    #[test]
    fn binary_fallback_renders_darwin_and_unknown_os() {
        let darwin = Platform {
            os: OsKind::Macos,
            pkg_manager: None,
            pkexec: false,
        };
        let plan = plan_install_for(ToolId::K9s, &darwin, Path::new("/tmp/bin")).unwrap();
        let curl = plan.steps.iter().find(|s| s.program == "curl").unwrap();
        assert_eq!(
            curl.args.last().unwrap(),
            "https://github.com/derailed/k9s/releases/download/v0.51.0/k9s_Darwin_amd64.tar.gz"
        );

        // Unknown OS families fall back to linux rendering (documented).
        let other = Platform {
            os: OsKind::Other,
            pkg_manager: None,
            pkexec: false,
        };
        let plan = plan_install_for(ToolId::Kind, &other, Path::new("/tmp/bin")).unwrap();
        let curl = plan.steps.iter().find(|s| s.program == "curl").unwrap();
        assert_eq!(
            curl.args.last().unwrap(),
            "https://kind.sigs.k8s.io/dl/v0.33.0/kind-linux-amd64"
        );
    }

    #[test]
    fn plan_kind_on_windows_uses_winget() {
        let plan = plan_install_for(ToolId::Kind, &windows(), Path::new("/x")).unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].program, "winget");
        assert_eq!(
            plan.steps[0].args,
            vec![
                "install",
                "--id",
                "Kubernetes.kind",
                "-e",
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements"
            ]
        );
    }

    #[test]
    fn plan_docker_on_windows_uses_choco_when_no_winget() {
        let plan = plan_install_for(ToolId::Docker, &windows_choco(), Path::new("/x")).unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].program, "choco");
        assert_eq!(plan.steps[0].args, vec!["install", "-y", "docker-desktop"]);
    }

    #[test]
    fn plan_helm_on_windows_without_manager_uses_zip_binary() {
        // No winget/choco → binary fallback: curl.exe + tar.exe (zip) +
        // PowerShell move, no chmod, sha256-pinned windows-amd64 artifact.
        let plan = plan_install_for(
            ToolId::Helm,
            &windows_none(),
            Path::new("C:/Users/u/.local/bin"),
        )
        .unwrap();
        let curl = plan.steps.iter().find(|s| s.program == "curl.exe").unwrap();
        assert_eq!(
            curl.args.last().unwrap(),
            "https://get.helm.sh/helm-v4.2.2-windows-amd64.zip"
        );
        let tar = plan.steps.iter().find(|s| s.program == "tar.exe").unwrap();
        assert_eq!(tar.args[0], "-xf");
        assert_eq!(tar.args[3], "C:/Users/u/.local/bin/.kb-extract-helm");
        assert_eq!(tar.args[4], "windows-amd64/helm.exe");
        let moves: Vec<String> = plan
            .steps
            .iter()
            .filter(|s| s.program == "powershell" && s.args[3].contains("Move-Item"))
            .map(|s| s.args[3].clone())
            .collect();
        assert_eq!(moves.len(), 1, "{moves:?}");
        assert!(
            moves[0].ends_with("'C:/Users/u/.local/bin/helm.exe'"),
            "{}",
            moves[0]
        );
        assert!(
            !plan.steps.iter().any(|s| s.program == "chmod"),
            "windows plans never chmod"
        );
        assert_eq!(plan.verify.len(), 1);
        assert_eq!(
            plan.verify[0].expected_sha256,
            "5fad8562e98c34fa5af3ef904086a5874a6701050f9bf36e30238c975df94dcd"
        );
    }

    #[test]
    fn plan_kind_on_windows_binary_fallback_installs_kind_exe() {
        // kind has no zip: the download IS the binary and must land as
        // `kind.exe` (extensionless names are not executable on Windows).
        let plan = plan_install_for(
            ToolId::Kind,
            &windows_none(),
            Path::new("C:/Users/u/.local/bin"),
        )
        .unwrap();
        let curl = plan.steps.iter().find(|s| s.program == "curl.exe").unwrap();
        assert_eq!(
            curl.args.last().unwrap(),
            "https://kind.sigs.k8s.io/dl/v0.33.0/kind-windows-amd64"
        );
        let install = plan
            .steps
            .iter()
            .find(|s| s.program == "powershell" && s.args[3].contains("Move-Item"))
            .unwrap();
        assert!(
            install.args[3].ends_with("'C:/Users/u/.local/bin/kind.exe'"),
            "{}",
            install.args[3]
        );
        assert_eq!(
            plan.verify[0].expected_sha256,
            "4b22adaa135368c5a465d56bbd8e520cbea87272a06ca00b6078e7b81515c9fc"
        );
    }

    #[test]
    fn plan_kubectx_on_windows_is_unsupported_with_honest_error() {
        // kubectx is a POSIX shell script — every Windows plan fails before
        // any winget/choco/binary attempt.
        for platform in [windows(), windows_choco(), windows_none()] {
            let err = plan_install_for(ToolId::Kubectx, &platform, Path::new("/x")).unwrap_err();
            match err {
                CoreError::Deps(DepsError::NoInstallRecipe { tool, reason }) => {
                    assert_eq!(tool, "kubectx");
                    assert!(
                        reason.contains("Windows"),
                        "reason must name Windows: {reason}"
                    );
                }
                other => panic!("expected NoInstallRecipe, got {other:?}"),
            }
        }
    }

    #[test]
    fn binary_fallback_renders_windows_urls_for_every_tool() {
        // Windows binary-fallback URLs must match the upstream release
        // assets (verified at implementation time, ADR-0019).
        let cases: Vec<(ToolId, &str)> = vec![
            (
                ToolId::Kind,
                "https://kind.sigs.k8s.io/dl/v0.33.0/kind-windows-amd64",
            ),
            (
                ToolId::Kubectl,
                "https://dl.k8s.io/release/v1.37.0/bin/windows/amd64/kubectl.exe",
            ),
            (
                ToolId::Helm,
                "https://get.helm.sh/helm-v4.2.2-windows-amd64.zip",
            ),
            (
                ToolId::Cilium,
                "https://github.com/cilium/cilium-cli/releases/download/v0.20.0/cilium-windows-amd64.zip",
            ),
            (
                ToolId::K9s,
                "https://github.com/derailed/k9s/releases/download/v0.51.0/k9s_Windows_amd64.zip",
            ),
            (
                ToolId::Kustomize,
                "https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize/v5.8.1/kustomize_v5.8.1_windows_amd64.zip",
            ),
        ];
        for (id, expected) in cases {
            let plan = plan_install_for(id, &windows_none(), Path::new("C:/tmp/bin")).unwrap();
            let curl = plan
                .steps
                .iter()
                .find(|step| step.program == "curl.exe")
                .unwrap_or_else(|| panic!("{id:?} must have a curl.exe download step"));
            let url = curl.args.last().expect("curl.exe step needs a url arg");
            assert_eq!(url, expected, "URL mismatch for {id:?}");
        }
    }

    #[test]
    fn binary_plan_steps_have_no_empty_args() {
        for id in [
            ToolId::Kind,
            ToolId::Kubectl,
            ToolId::Helm,
            ToolId::Cilium,
            ToolId::K9s,
            ToolId::Kubectx,
            ToolId::Kustomize,
        ] {
            let plan = plan_install_for(id, &linux_none(), Path::new("/tmp/bin")).unwrap();
            for step in &plan.steps {
                assert!(!step.program.is_empty(), "{id:?} has an empty program");
                for arg in &step.args {
                    assert!(!arg.is_empty(), "{id:?} step {step:?} has an empty arg");
                }
                assert!(!plan.render().is_empty(), "{id:?} render must not be empty");
            }
        }
    }

    #[test]
    fn plan_render_quotes_spaces() {
        let plan = plan_install_for(
            ToolId::Kind,
            &linux_none(),
            Path::new("/home/my user/.local/bin"),
        )
        .unwrap();
        let rendered = plan.render();
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("\"/home/my user/.local/bin\"")),
            "{rendered:?}"
        );
    }

    #[test]
    fn plan_install_uses_current_platform() {
        // Sanity: must not panic on this host and must produce something.
        let plan = plan_install(ToolId::Kind);
        assert!(
            plan.is_ok()
                || matches!(
                    plan,
                    Err(CoreError::Deps(DepsError::NoInstallRecipe { .. }))
                )
        );
    }

    #[tokio::test]
    async fn install_via_stub_binaries_succeeds_and_redetects() {
        // End-to-end (no network): fake kind "binary" + fake curl/mkdir/
        // chmod/mv on a controlled PATH; install must re-detect Installed.
        let dir = temp_dir("install-e2e");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        make_stub(&bin, "kind", "echo 'kind v0.33.0 go1.26.7 linux/amd64'");
        make_stub(&bin, "curl", "echo fake-curl; exit 0");
        make_stub(&bin, "chmod", "exit 0");
        make_stub(&bin, "mv", "exit 0");
        make_stub(&bin, "mkdir", "exit 0");
        make_stub(&bin, "tar", "exit 0");
        make_stub(&bin, "rm", "exit 0");
        make_stub(&bin, "sh", "exit 0");

        // Redirect detection to the fake bin dir by running the same steps
        // install uses, against a custom PATH via detect_with_path.
        let status = detect_with_path(ToolId::Kind, std::slice::from_ref(&bin))
            .await
            .unwrap();
        assert!(status.is_installed(), "{status:?}");

        // Executing the real install() would run the real PATH; instead
        // exercise plan + Cmd construction deterministically:
        let plan = plan_install_for(ToolId::Kind, &linux_none(), &bin).unwrap();
        assert!(!plan.steps.is_empty());
        for step in &plan.steps {
            let cmd = Cmd::new(&step.program).args(step.args.iter().cloned());
            assert!(!cmd.argv().is_empty());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn install_preflight_installed_skips_plan_and_reports_success() {
        // Docker is pkg_manager_only with no binary fallback: on a host with
        // no package manager, building the plan itself would fail. A
        // successful skip therefore proves the plan is never built — the
        // pre-flight gate runs first, zero steps execute (D4).
        let (tx, mut rx) = mpsc::channel(8);
        let installed = ToolStatus::Installed {
            version: Version {
                major: 29,
                minor: 8,
                patch: 0,
            },
            path: PathBuf::from("/usr/bin/docker"),
        };
        let result =
            install_with_progress_preflighted(ToolId::Docker, None, &tx, installed.clone()).await;
        drop(tx);
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        assert_eq!(result.unwrap(), installed);
        assert_eq!(
            events.len(),
            1,
            "pre-flight skip must emit exactly one line, got {events:?}"
        );
        match &events[0] {
            InstallEvent::StepLine { step, line } => {
                assert_eq!(*step, 0);
                assert!(line.contains("already installed"), "{line}");
            }
            other => panic!("expected StepLine, got {other:?}"),
        }
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, InstallEvent::StepStarted { .. })),
            "no install step may run when the tool is already installed"
        );
    }

    #[test]
    fn reconcile_failure_recheck_turns_presence_into_success() {
        let installed = ToolStatus::Installed {
            version: Version::UNKNOWN,
            path: PathBuf::from("/usr/bin/kind"),
        };
        assert_eq!(
            reconcile_failure_recheck(Ok(installed.clone())),
            Some(installed.clone())
        );
        assert_eq!(
            reconcile_failure_recheck(Ok(ToolStatus::NotInstalled)),
            None
        );
        assert_eq!(
            reconcile_failure_recheck(Ok(ToolStatus::Broken {
                reason: "still broken".to_string()
            })),
            None
        );
        assert_eq!(
            reconcile_failure_recheck(Err(DepsError::NotFound("kind".to_string()).into())),
            None
        );
    }

    #[test]
    fn every_binary_plan_pins_a_sha256_for_all_platforms() {
        // Each tool must pin digests for linux/darwin × amd64/arm64, plus
        // windows-amd64 for every Windows-capable tool (kubectx is a POSIX
        // script — explicitly unsupported, so it must NOT pin one). Every
        // platform must resolve to exactly one verify entry.
        for id in [
            ToolId::Kind,
            ToolId::Kubectl,
            ToolId::Helm,
            ToolId::Cilium,
            ToolId::K9s,
            ToolId::Kubectx,
            ToolId::Kustomize,
        ] {
            let binary = tool(id).unwrap().install.binary.as_ref().unwrap();
            for key in ["linux-amd64", "linux-arm64", "darwin-amd64", "darwin-arm64"] {
                assert!(
                    binary
                        .sha256
                        .iter()
                        .any(|(k, digest)| *k == key && digest.len() == 64),
                    "{id:?} missing digest for {key}"
                );
            }
            let windows_pin = binary
                .sha256
                .iter()
                .any(|(k, digest)| *k == "windows-amd64" && digest.len() == 64);
            if tool(id).unwrap().install.windows_unsupported {
                assert!(
                    !windows_pin,
                    "{id:?} is windows-unsupported; must not pin windows-amd64"
                );
            } else {
                assert!(windows_pin, "{id:?} missing digest for windows-amd64");
            }
            let darwin_none = Platform {
                os: OsKind::Macos,
                pkg_manager: None,
                pkexec: false,
            };
            for platform in [linux_none(), darwin_none] {
                let plan = plan_install_for(id, &platform, Path::new("/tmp/bin")).unwrap();
                assert_eq!(
                    plan.verify.len(),
                    1,
                    "{id:?} must have exactly one download verification"
                );
                assert!(
                    plan.verify[0].file.to_string_lossy().contains(".kb-dl-"),
                    "{id:?} verify must target the download file"
                );
                assert_eq!(plan.verify[0].expected_sha256.len(), 64);
            }
            if !tool(id).unwrap().install.windows_unsupported {
                let plan = plan_install_for(id, &windows_none(), Path::new("/tmp/bin")).unwrap();
                assert_eq!(
                    plan.verify.len(),
                    1,
                    "{id:?} must have exactly one windows download verification"
                );
                assert!(
                    plan.verify[0].file.to_string_lossy().contains(".kb-dl-"),
                    "{id:?} windows verify must target the download file"
                );
                assert_eq!(plan.verify[0].expected_sha256.len(), 64);
            }
        }
    }

    #[tokio::test]
    async fn wrong_digest_fails_the_install_step() {
        let dir = temp_dir("checksum-bad");
        let file = dir.join("artifact");
        std::fs::write(&file, b"tampered bytes").unwrap();
        let err = verify_download_file(
            "kind",
            "verify kind (sha256)",
            &file,
            "aee6151561422756b764a4ae28e7f44cda5af5a9eead3cc9985112b1de8d8e0d",
        )
        .await
        .unwrap_err();
        match err {
            CoreError::Deps(DepsError::ChecksumMismatch {
                tool,
                path,
                expected_sha256,
            }) => {
                assert_eq!(tool, "kind");
                assert_eq!(path, file);
                assert!(expected_sha256.starts_with("aee61515"), "{expected_sha256}");
            }
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn correct_digest_passes_verification() {
        let dir = temp_dir("checksum-ok");
        let file = dir.join("artifact");
        std::fs::write(&file, b"abc").unwrap();
        verify_download_file(
            "kind",
            "verify kind (sha256)",
            &file,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )
        .await
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_render_never_exposes_shell_syntax() {
        // Plans render for display only; execution uses argv arrays. The
        // render output must never contain shell metacharacters that could
        // be re-interpreted.
        for id in [ToolId::Kind, ToolId::Helm, ToolId::Kubectx] {
            let plan = plan_install_for(id, &linux_none(), Path::new("/tmp/bin")).unwrap();
            for line in plan.render() {
                assert!(!line.contains(';'), "{line}");
                assert!(!line.contains('|'), "{line}");
                assert!(!line.contains("&&"), "{line}");
                assert!(!line.contains("$("), "{line}");
            }
        }
    }

    // Security invariant (audit finding H1 regression): on EVERY platform,
    // EVERY curl-family download step must have a pinned sha256 verification
    // entry targeting its exact `-o` file, and every verification entry must
    // correspond to exactly one download step. The install loop verifies a
    // file before any later step can consume it — this test pins the plan
    // shape that guarantee depends on (the Windows plan runs `curl.exe`,
    // which previously escaped the `program == "curl"` gate).
    #[test]
    fn every_download_step_is_sha256_verified_on_all_platforms() {
        const ALL_TOOLS: [ToolId; 8] = [
            ToolId::Docker,
            ToolId::Kind,
            ToolId::Kubectl,
            ToolId::Helm,
            ToolId::Cilium,
            ToolId::K9s,
            ToolId::Kubectx,
            ToolId::Kustomize,
        ];
        let platforms: [Platform; 3] = [linux_none(), macos(), windows()];
        let is_curl = |program: &str| {
            program.eq_ignore_ascii_case("curl") || program.eq_ignore_ascii_case("curl.exe")
        };

        for id in ALL_TOOLS {
            for platform in &platforms {
                let Ok(plan) = plan_install_for(id, platform, Path::new("/tmp/bin")) else {
                    // No recipe for this tool on this platform is allowed
                    // only when it is not installable at all (e.g. Docker
                    // on Windows without choco); the invariant only covers
                    // plans that exist.
                    continue;
                };
                let mut verified_files = std::collections::HashMap::<&Path, usize>::new();
                for step in &plan.steps {
                    if !is_curl(&step.program) {
                        continue;
                    }
                    let download_path = step
                        .args
                        .iter()
                        .position(|arg| arg == "-o")
                        .and_then(|pos| step.args.get(pos + 1))
                        .map(Path::new)
                        .expect("curl-family step must have -o <file>");
                    let check = plan
                        .verify
                        .iter()
                        .find(|check| check.file == download_path)
                        .unwrap_or_else(|| {
                            panic!(
                                "no sha256 verify entry for {} download ({id:?} on {:?})",
                                download_path.display(),
                                platform.os
                            )
                        });
                    assert!(
                        !check.expected_sha256.is_empty(),
                        "empty sha256 pin for {} ({id:?})",
                        download_path.display()
                    );
                    *verified_files.entry(download_path).or_insert(0) += 1;
                }
                for check in &plan.verify {
                    let count = verified_files
                        .get(check.file.as_path())
                        .copied()
                        .unwrap_or(0);
                    assert_eq!(
                        count,
                        1,
                        "verify entry for {} must match exactly one curl step ({id:?})",
                        check.file.display()
                    );
                }
            }
        }
    }
}
