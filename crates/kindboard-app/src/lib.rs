//! kindboard-app: the eframe/egui desktop frontend of kindboard.
//!
//! The app is a thin shell around `kindboard-core`: it enqueues commands on
//! a bounded crossbeam bus ([`bus::CoreCommand`]), renders events streamed
//! back ([`bus::CoreEvent`]), and owns the worker thread that runs the
//! core's tokio runtime ([`worker`]). All subprocess/kubeconfig/k8s work
//! happens in core; the UI never blocks and never talks to the outside
//! world directly.
//!
//! Layout:
//! - [`app`] — `KindboardApp` (eframe::App), event routing, tab bar,
//!   generation bookkeeping.
//! - [`bus`] — the command/event contract between UI and core.
//! - [`worker`] — the background tokio thread executing core calls.
//! - [`views`] — overview, create wizard, per-cluster tabs (topology
//!   diagram + logs), dependency panel, about/screenshot.

#![forbid(unsafe_code)]

pub mod app;
pub mod bus;
pub mod icons;
pub mod theme;
pub mod util;
pub mod views;
pub mod worker;

pub use app::KindboardApp;
pub use bus::{Buses, CoreCommand, CoreEvent};
