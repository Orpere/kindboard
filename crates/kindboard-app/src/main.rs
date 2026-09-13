//! kindboard binary: minimal CLI handling (--version/--help), window icon
//! loading, worker-thread startup, eframe bootstrap and shutdown.

use std::sync::Arc;

use eframe::egui;
use kindboard_app::{Buses, CoreCommand, KindboardApp, worker};

/// Logger teeing timestamped records to stderr and (optionally) a file
/// (ADR-0013).
struct AppLogger {
    /// Optional log file: every record also gets appended here (flushed per
    /// record). `Mutex` keeps `AppLogger` `Sync` for `log::set_logger`.
    file: Option<std::sync::Mutex<std::fs::File>>,
}

impl log::Log for AppLogger {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        eprintln!("{}", format_record(record, false));
        if let Some(file) = &self.file
            && let Ok(mut file) = file.lock()
        {
            let with_target = record.level() <= log::Level::Debug;
            let line = format_record(record, with_target);
            let _ = std::io::Write::write_all(&mut *file, line.as_bytes());
            let _ = std::io::Write::flush(&mut *file);
        }
    }

    fn flush(&self) {}
}

/// Format one record as `kindboard [HH:MM:SS.mmm] LEVEL: msg`, optionally
/// appending the record's target (for verbose file logs).
fn format_record(record: &log::Record<'_>, with_target: bool) -> String {
    let time = chrono::Local::now().format("%H:%M:%S%.3f");
    let line = format!("kindboard [{time}] {}: {}", record.level(), record.args());
    if with_target {
        format!("{line} [{}]", record.target())
    } else {
        line
    }
}

fn print_help() {
    println!(
        "kindboard {} — manage kind clusters from a desktop app",
        env!("CARGO_PKG_VERSION")
    );
    println!("usage: kindboard [--version] [--help]");
    println!("       kindboard --screenshot <file.png>");
    println!("       kindboard --screenshot-every <secs> [--screenshot-dir <dir>]");
    println!();
    println!("troubleshooting:");
    println!(
        "  --detach               run the GUI in the background (new session, stdio to /dev/null)"
    );
    println!("  -v, --verbose          raise the log level (repeatable: -v debug, -vv trace)");
    println!("  --log-file <path>      also write log records to <path>");
    println!(
        "                         (default in detach mode: <data_dir>/kindboard/kindboard.log)"
    );
    println!("all other arguments are ignored");
}

/// Dev/CI screenshot arguments parsed from the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ScreenshotArgs {
    /// One capture, then exit.
    Once {
        path: std::path::PathBuf,
        /// Seconds to wait (for async state like tool detection) before
        /// capturing.
        delay_secs: u64,
    },
    /// Periodic captures into a directory.
    Every {
        interval: std::time::Duration,
        dir: std::path::PathBuf,
    },
}

/// Parse `--screenshot` / `--screenshot-every` (+ `--screenshot-dir`) from
/// argv. `None` when no screenshot flag is present; `Err` for invalid
/// combinations or values.
fn parse_screenshot_args(args: &[String]) -> Result<Option<ScreenshotArgs>, String> {
    let mut once: Option<std::path::PathBuf> = None;
    let mut every: Option<u64> = None;
    let mut dir: Option<std::path::PathBuf> = None;
    let mut delay: Option<u64> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--screenshot" => {
                let path = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| "--screenshot requires a file path".to_string())?;
                once = Some(std::path::PathBuf::from(path));
                index += 2;
            }
            "--screenshot-every" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| "--screenshot-every requires seconds".to_string())?;
                let secs = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid seconds: {value}"))?;
                if secs == 0 {
                    return Err("--screenshot-every needs at least 1 second".to_string());
                }
                every = Some(secs);
                index += 2;
            }
            "--screenshot-dir" => {
                let path = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| "--screenshot-dir requires a directory".to_string())?;
                dir = Some(std::path::PathBuf::from(path));
                index += 2;
            }
            "--screenshot-delay" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| "--screenshot-delay requires seconds".to_string())?;
                let secs = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid seconds: {value}"))?;
                delay = Some(secs);
                index += 2;
            }
            _ => index += 1,
        }
    }
    match (once, every) {
        (Some(_), Some(_)) => {
            Err("--screenshot and --screenshot-every are mutually exclusive".to_string())
        }
        (Some(path), None) => {
            if dir.is_some() {
                return Err("--screenshot-dir requires --screenshot-every".to_string());
            }
            Ok(Some(ScreenshotArgs::Once {
                path,
                delay_secs: delay.unwrap_or(0),
            }))
        }
        (None, Some(secs)) => {
            if delay.is_some() {
                return Err("--screenshot-delay only applies to --screenshot".to_string());
            }
            let dir = dir
                .or_else(|| {
                    std::env::var_os("KINDBOARD_SCREENSHOT_DIR").map(std::path::PathBuf::from)
                })
                .unwrap_or_else(|| std::path::PathBuf::from("screenshots"));
            Ok(Some(ScreenshotArgs::Every {
                interval: std::time::Duration::from_secs(secs),
                dir,
            }))
        }
        (None, None) => {
            if dir.is_some() {
                Err("--screenshot-dir requires --screenshot-every".to_string())
            } else if delay.is_some() {
                Err("--screenshot-delay requires --screenshot".to_string())
            } else {
                Ok(None)
            }
        }
    }
}

/// Troubleshooting options parsed from the command line (ADR-0013).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct TroubleshootArgs {
    /// Re-exec the GUI in a new session with stdio to /dev/null.
    detach: bool,
    /// Repeatable verbosity: 0→Warn, 1→Debug, ≥2→Trace.
    verbose: u8,
    /// Optional file to tee log records into.
    log_file: Option<std::path::PathBuf>,
}

/// Whether `arg` is `-v`-style (and how many `v`s carry, e.g. `-vv` = 2).
fn is_verbose_flag(arg: &str) -> bool {
    arg == "--verbose"
        || (arg.starts_with("-v") && !arg.starts_with("--") && arg[1..].chars().all(|c| c == 'v'))
}

/// The verbosity count of one flag (`-v`→1, `-vv`→2, `--verbose`→1).
fn verbose_count(arg: &str) -> u8 {
    if arg == "--verbose" {
        1
    } else {
        (arg.len() - 1) as u8
    }
}

/// Parse `--detach` / `-v` | `--verbose` / `--log-file <path>` from argv.
/// Unknown arguments are skipped (backward compat with the existing parser).
fn parse_troubleshoot_args(args: &[String]) -> Result<TroubleshootArgs, String> {
    let mut detach = false;
    let mut verbose: u8 = 0;
    let mut log_file: Option<std::path::PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--detach" => {
                detach = true;
                index += 1;
            }
            arg if is_verbose_flag(arg) => {
                verbose = verbose.saturating_add(verbose_count(arg));
                index += 1;
            }
            "--log-file" => {
                let path = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| "--log-file requires a path".to_string())?;
                log_file = Some(std::path::PathBuf::from(path));
                index += 2;
            }
            _ => index += 1,
        }
    }
    Ok(TroubleshootArgs {
        detach,
        verbose,
        log_file,
    })
}

/// Map verbosity to the maximum log level gated (`0`→Warn, `1`→Debug,
/// `≥2`→Trace).
fn level_for(verbose: u8) -> log::LevelFilter {
    match verbose {
        0 => log::LevelFilter::Warn,
        1 => log::LevelFilter::Debug,
        _ => log::LevelFilter::Trace,
    }
}

/// Resolve the effective log-file path: an explicit `--log-file` wins; in
/// `--detach` mode with no explicit file, default to
/// `<data_dir>/kindboard/kindboard.log`; otherwise no file.
fn resolve_log_file(
    log_file: &Option<std::path::PathBuf>,
    detach: bool,
) -> Option<std::path::PathBuf> {
    if let Some(path) = log_file {
        return Some(path.clone());
    }
    if detach {
        return dirs::data_dir().map(|dir| dir.join("kindboard").join("kindboard.log"));
    }
    None
}

/// `--detach` combined with any `--screenshot*` flag is a usage error: the
/// parent would exit before the child could capture.
fn detach_screenshot_conflict(
    detach: bool,
    screenshot: &Option<ScreenshotArgs>,
) -> Result<(), String> {
    if detach && screenshot.is_some() {
        return Err("--detach cannot be combined with --screenshot flags".to_string());
    }
    Ok(())
}

/// Build (but do NOT spawn) the re-exec command for `--detach`: current
/// executable, `--detach` stripped from argv, `KINDBOARD_DETACHED=1`, stdio
/// nulled, and (unix) a new session via `setsid`.
#[cfg(unix)]
fn build_detach_command(args: &[String]) -> std::process::Command {
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("current_exe"));
    cmd.args(
        args.iter()
            .filter(|arg| arg.as_str() != "--detach")
            .map(String::as_str),
    );
    cmd.env("KINDBOARD_DETACHED", "1");
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd
}

/// Spawn the detach re-exec (unix-only; stdio is nulled so the child runs
/// with no terminal).
#[cfg(unix)]
fn detach_self(args: &[String]) -> std::io::Result<std::process::Child> {
    build_detach_command(args).spawn()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("kindboard {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return;
    }
    let screenshot = match parse_screenshot_args(&args) {
        Ok(mode) => mode,
        Err(err) => {
            eprintln!("kindboard: {err}");
            std::process::exit(2);
        }
    };
    let troubleshoot = match parse_troubleshoot_args(&args) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("kindboard: {err}");
            std::process::exit(2);
        }
    };
    if let Err(err) = detach_screenshot_conflict(troubleshoot.detach, &screenshot) {
        eprintln!("kindboard: {err}");
        std::process::exit(2);
    }

    // Logging setup: verbosity level + optional file tee. The log path is
    // resolved first because the detach branch prints it before re-exec.
    let log_path = resolve_log_file(&troubleshoot.log_file, troubleshoot.detach);
    let logger_file = match &log_path {
        Some(path) => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                && let Err(err) = std::fs::create_dir_all(parent)
                && troubleshoot.log_file.is_some()
            {
                eprintln!(
                    "kindboard: cannot create log directory {}: {err}",
                    parent.display()
                );
                std::process::exit(2);
            }
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                Ok(file) => Some(file),
                Err(err) => {
                    if troubleshoot.log_file.is_some() {
                        eprintln!("kindboard: cannot open log file {}: {err}", path.display());
                        std::process::exit(2);
                    }
                    None
                }
            }
        }
        None => None,
    };
    let logger = Box::new(AppLogger {
        file: logger_file.map(std::sync::Mutex::new),
    });
    let _ = log::set_logger(Box::leak(logger));
    log::set_max_level(level_for(troubleshoot.verbose));

    // --detach: re-exec the GUI in a new session, then exit 0. The child
    // (KINDBOARD_DETACHED=1, --detach stripped from argv) never re-detaches.
    #[cfg(unix)]
    if troubleshoot.detach && std::env::var_os("KINDBOARD_DETACHED").is_none() {
        let child = match detach_self(&args) {
            Ok(child) => child,
            Err(err) => {
                eprintln!("kindboard: cannot start in background: {err}");
                std::process::exit(1);
            }
        };
        println!(
            "kindboard {} started in background (pid {})",
            env!("CARGO_PKG_VERSION"),
            child.id()
        );
        if let Some(path) = &log_path {
            println!("logs: {}", path.display());
        }
        return;
    }
    #[cfg(not(unix))]
    if troubleshoot.detach {
        eprintln!("kindboard: --detach is not supported on this platform");
        std::process::exit(2);
    }

    // Buses: UI ↔ core worker.
    let buses = Buses::new();
    let shutdown_tx = buses.cmd_tx.clone();
    let worker_handle = worker::spawn(buses.cmd_rx, buses.event_tx);

    // Window icon (tolerated when assets/icons/kindboard-64.png is absent).
    let icon: Option<Arc<egui::viewport::IconData>> = kindboard_app::icons::load_window_icon();
    // Dev/CI: override the initial window size via KINDBOARD_WINDOW_SIZE
    // ("WIDTHxHEIGHT") so the screenshot harness can verify every view at
    // any frame size (R3). Unset → the 1280x800 default.
    let window_size = std::env::var("KINDBOARD_WINDOW_SIZE")
        .ok()
        .and_then(|value| {
            let (w, h) = value.split_once('x')?;
            Some(egui::vec2(w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or(egui::vec2(1280.0, 800.0));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("kindboard")
        .with_inner_size(window_size)
        .with_min_inner_size([640.0, 480.0])
        .with_resizable(true);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let cmd_tx = buses.cmd_tx;
    let event_rx = buses.event_rx;
    // Read the persisted theme once, synchronously, before the window
    // exists — no flash of the default theme for light-theme users, and
    // the UI thread never blocks after startup.
    let initial_theme = kindboard_app::theme::load_persisted_theme();
    let run_result = eframe::run_native(
        "kindboard",
        options,
        Box::new(move |cc| {
            let mut app = KindboardApp::new(cc, cmd_tx, event_rx, initial_theme);
            if let Some(mode) = screenshot {
                app = app.with_screenshot(match mode {
                    ScreenshotArgs::Once { path, delay_secs } => {
                        kindboard_app::ScreenshotMode::Once {
                            path,
                            delay: std::time::Duration::from_secs(delay_secs),
                        }
                    }
                    ScreenshotArgs::Every { interval, dir } => {
                        kindboard_app::ScreenshotMode::Every { interval, dir }
                    }
                });
            }
            Ok(Box::new(app))
        }),
    );

    // Clean shutdown: stop the worker loop, then join the thread so its
    // subprocess kill ladder completes before the process exits. A blocking
    // `send` (not `try_send`) guarantees Shutdown is delivered even when
    // the command queue is full — a dropped Shutdown would hang `join`
    // forever with the window already gone.
    let _ = shutdown_tx.send(CoreCommand::Shutdown);
    let _ = worker_handle.join();

    if let Err(err) = run_result {
        eprintln!("kindboard: {err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn no_screenshot_flags_is_none() {
        assert_eq!(parse_screenshot_args(&strs(&[])).unwrap(), None);
        assert_eq!(parse_screenshot_args(&strs(&["--version"])).unwrap(), None);
    }

    #[test]
    fn once_parses_path() {
        assert_eq!(
            parse_screenshot_args(&strs(&["--screenshot", "out.png"])).unwrap(),
            Some(ScreenshotArgs::Once {
                path: std::path::PathBuf::from("out.png"),
                delay_secs: 0,
            })
        );
    }

    #[test]
    fn once_parses_delay() {
        assert_eq!(
            parse_screenshot_args(&strs(&[
                "--screenshot",
                "out.png",
                "--screenshot-delay",
                "8"
            ]))
            .unwrap(),
            Some(ScreenshotArgs::Once {
                path: std::path::PathBuf::from("out.png"),
                delay_secs: 8,
            })
        );
    }

    #[test]
    fn delay_without_once_fails() {
        assert!(parse_screenshot_args(&strs(&["--screenshot-delay", "3"])).is_err());
        assert!(
            parse_screenshot_args(&strs(&[
                "--screenshot-every",
                "2",
                "--screenshot-delay",
                "3"
            ]))
            .is_err()
        );
    }

    #[test]
    fn every_parses_seconds_and_default_dir() {
        let mode = parse_screenshot_args(&strs(&["--screenshot-every", "3"])).unwrap();
        match mode {
            Some(ScreenshotArgs::Every { interval, dir }) => {
                assert_eq!(interval, std::time::Duration::from_secs(3));
                assert_eq!(dir, std::path::PathBuf::from("screenshots"));
            }
            other => panic!("expected Every, got {other:?}"),
        }
    }

    #[test]
    fn every_uses_explicit_dir() {
        let mode = parse_screenshot_args(&strs(&[
            "--screenshot-every",
            "5",
            "--screenshot-dir",
            "/tmp/x",
        ]))
        .unwrap();
        match mode {
            Some(ScreenshotArgs::Every { interval, dir }) => {
                assert_eq!(interval, std::time::Duration::from_secs(5));
                assert_eq!(dir, std::path::PathBuf::from("/tmp/x"));
            }
            other => panic!("expected Every, got {other:?}"),
        }
    }

    #[test]
    fn screenshot_without_value_fails() {
        assert!(parse_screenshot_args(&strs(&["--screenshot"])).is_err());
    }

    #[test]
    fn every_with_zero_seconds_fails() {
        assert!(parse_screenshot_args(&strs(&["--screenshot-every", "0"])).is_err());
    }

    #[test]
    fn every_with_non_numeric_seconds_fails() {
        assert!(parse_screenshot_args(&strs(&["--screenshot-every", "soon"])).is_err());
    }

    #[test]
    fn once_and_every_are_mutually_exclusive() {
        assert!(
            parse_screenshot_args(&strs(&["--screenshot", "a.png", "--screenshot-every", "2"]))
                .is_err()
        );
    }

    #[test]
    fn dir_without_every_fails() {
        assert!(parse_screenshot_args(&strs(&["--screenshot-dir", "/tmp/x"])).is_err());
    }

    // ---- troubleshooting args (ADR-0013) ----

    #[test]
    fn verbose_counts() {
        assert_eq!(parse_troubleshoot_args(&strs(&[])).unwrap().verbose, 0);
        assert_eq!(parse_troubleshoot_args(&strs(&["-v"])).unwrap().verbose, 1);
        assert_eq!(
            parse_troubleshoot_args(&strs(&["--verbose"]))
                .unwrap()
                .verbose,
            1
        );
        assert_eq!(parse_troubleshoot_args(&strs(&["-vv"])).unwrap().verbose, 2);
        assert_eq!(
            parse_troubleshoot_args(&strs(&["-v", "--verbose", "-v"]))
                .unwrap()
                .verbose,
            3
        );
    }

    #[test]
    fn level_for_maps_verbosity() {
        assert_eq!(level_for(0), log::LevelFilter::Warn);
        assert_eq!(level_for(1), log::LevelFilter::Debug);
        assert_eq!(level_for(2), log::LevelFilter::Trace);
        assert_eq!(level_for(5), log::LevelFilter::Trace);
    }

    #[test]
    fn detach_parses() {
        assert!(
            parse_troubleshoot_args(&strs(&["--detach"]))
                .unwrap()
                .detach
        );
        assert!(!parse_troubleshoot_args(&strs(&[])).unwrap().detach);
    }

    #[test]
    fn log_file_requires_a_value() {
        assert!(parse_troubleshoot_args(&strs(&["--log-file"])).is_err());
        assert!(parse_troubleshoot_args(&strs(&["--log-file", "--detach"])).is_err());
    }

    #[test]
    fn log_file_captures_path() {
        let args = parse_troubleshoot_args(&strs(&["--log-file", "/tmp/x"])).unwrap();
        assert_eq!(args.log_file, Some(std::path::PathBuf::from("/tmp/x")));
        assert!(!args.detach, "log-file alone must not imply --detach");
    }

    #[test]
    fn detach_conflicts_with_screenshot() {
        let once = parse_screenshot_args(&strs(&["--screenshot", "x.png"])).unwrap();
        assert!(detach_screenshot_conflict(true, &once).is_err());
        assert!(detach_screenshot_conflict(false, &once).is_ok());
        assert!(detach_screenshot_conflict(true, &None).is_ok());
    }

    #[test]
    fn resolve_log_file_explicit_wins() {
        let explicit = std::path::PathBuf::from("/tmp/custom.log");
        assert_eq!(
            resolve_log_file(&Some(explicit.clone()), false),
            Some(explicit.clone())
        );
        assert_eq!(
            resolve_log_file(&Some(explicit.clone()), true),
            Some(explicit)
        );
    }

    #[test]
    fn resolve_log_file_defaults_in_detach_only() {
        assert_eq!(resolve_log_file(&None, false), None);
        match dirs::data_dir() {
            Some(data_dir) => assert_eq!(
                resolve_log_file(&None, true),
                Some(data_dir.join("kindboard").join("kindboard.log"))
            ),
            None => assert_eq!(resolve_log_file(&None, true), None),
        }
    }

    #[cfg(unix)]
    #[test]
    fn detach_command_strips_flag_nulls_stdio_sets_env() {
        let cmd = build_detach_command(&strs(&["--detach", "-v", "--screenshot", "x.png"]));
        let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert!(
            argv.iter().all(|arg| arg.to_str() != Some("--detach")),
            "argv must not contain --detach: {argv:?}"
        );
        assert!(argv.iter().any(|arg| arg.to_str() == Some("-v")));
        assert!(argv.iter().any(|arg| arg.to_str() == Some("--screenshot")));
        assert!(argv.iter().any(|arg| arg.to_str() == Some("x.png")));
        assert_eq!(
            detach_env(&cmd),
            Some(std::ffi::OsString::from("1")),
            "KINDBOARD_DETACHED must be set to 1"
        );
        // Stable std exposes no `get_stdin/get_stdout/get_stderr`, so null
        // stdio is verified behaviorally: a child run with `output()` must
        // capture nothing (Stdio::null is respected; a pipe would capture).
        let out = build_detach_command(&strs(&["--detach", "--version"]))
            .output()
            .unwrap();
        assert!(
            out.stdout.is_empty(),
            "detached stdout must be /dev/null, got {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            out.stderr.is_empty(),
            "detached stderr must be /dev/null, got {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        // stdin is not piped when spawned.
        let mut child = build_detach_command(&strs(&["--detach", "--version"]))
            .spawn()
            .unwrap();
        assert!(child.stdin.is_none(), "detached stdin must not be piped");
        let _ = child.wait();
    }

    #[cfg(unix)]
    fn detach_env(cmd: &std::process::Command) -> Option<std::ffi::OsString> {
        cmd.get_envs()
            .find(|(key, _)| key.to_str() == Some("KINDBOARD_DETACHED"))
            .and_then(|(_, value)| value)
            .map(std::ffi::OsString::from)
    }

    #[test]
    fn format_record_includes_metadata_and_target() {
        let record = log::Record::builder()
            .args(format_args!("hello world"))
            .level(log::Level::Warn)
            .target("kindboard_app::worker")
            .build();
        let line = format_record(&record, false);
        assert!(line.starts_with("kindboard ["), "line: {line}");
        assert!(line.contains("WARN"), "line: {line}");
        assert!(line.contains("hello world"), "line: {line}");
        assert!(!line.contains("kindboard_app::worker"), "line: {line}");
        let line = format_record(&record, true);
        assert!(line.contains("kindboard_app::worker"), "line: {line}");
    }
}
