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
    println!("all other arguments are ignored");
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
        Box::new(move |cc| Ok(Box::new(KindboardApp::new(cc, cmd_tx, event_rx)))),
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
