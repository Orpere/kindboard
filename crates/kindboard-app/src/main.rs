//! kindboard binary: minimal CLI handling (--version/--help), window icon
//! loading, worker-thread startup, eframe bootstrap and shutdown.

use std::sync::Arc;

use eframe::egui;
use kindboard_app::{Buses, CoreCommand, KindboardApp, worker};

/// Minimal logger writing warnings/errors to stderr (no dependencies).
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        eprintln!("kindboard {}: {}", record.level(), record.args());
    }

    fn flush(&self) {}
}

fn print_help() {
    println!(
        "kindboard {} — manage kind clusters from a desktop app",
        env!("CARGO_PKG_VERSION")
    );
    println!("usage: kindboard [--version] [--help]");
    println!("       kindboard --screenshot <file.png>");
    println!("       kindboard --screenshot-every <secs> [--screenshot-dir <dir>]");
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

fn main() {
    // Minimal arg handling: only --version/--help are recognized.
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

    // Logging setup (warnings only; the icon-not-found notice logs once).
    let _ = log::set_logger(&StderrLogger);
    log::set_max_level(log::LevelFilter::Warn);

    // Buses: UI ↔ core worker.
    let buses = Buses::new();
    let shutdown_tx = buses.cmd_tx.clone();
    let worker_handle = worker::spawn(buses.cmd_rx, buses.event_tx);

    // Window icon (tolerated when assets/icons/kindboard-64.png is absent).
    let icon: Option<Arc<egui::viewport::IconData>> = kindboard_app::icons::load_window_icon();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("kindboard")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([960.0, 600.0])
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
    let run_result = eframe::run_native(
        "kindboard",
        options,
        Box::new(move |cc| {
            let mut app = KindboardApp::new(cc, cmd_tx, event_rx);
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
}
