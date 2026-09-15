//! The kindboard application: eframe shell, tab bar, event routing and
//! generation bookkeeping.
//!
//! The app owns all UI state and the two buses. Every frame it:
//! 1. drains the event bus (`try_iter`, never blocking) into state,
//! 2. handles keyboard shortcuts and screenshot replies,
//! 3. renders the active view (which returns commands),
//! 4. enqueues those commands (`try_send`, never blocking).
//!
//! Generation discipline: [`Self::next_op_gen`] stamps create/destroy/
//! recreate commands; op panels and tabs drop events carrying other
//! generations.

use std::collections::HashMap;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TrySendError};
use eframe::egui::{self, RichText};
use kindboard_core::ClusterState;

use crate::bus::{CoreCommand, CoreEvent, OpGen};
use crate::theme;
use crate::views::about;
use crate::views::cluster::{self, ClusterTab, TabCmd};
use crate::views::ops::{OpKind, OpPanel};
use crate::views::overview::{self, OverviewCmd, OverviewState};
use crate::views::wizard::{self, WizardAction, WizardState};

/// Frame tick while something is running (progress, logs, auto-refresh).
const ACTIVE_TICK: Duration = Duration::from_millis(100);

/// Repaint cadence while tool detection is in flight (keeps the detect
/// watchdog ticking).
const DETECT_TICK: Duration = Duration::from_secs(2);

/// A detection run taking longer than this is considered stalled and is
/// re-issued (bounded). Detection probes tools concurrently; the worst
/// legitimate run is a single 30 s per-tool timeout plus exec overhead.
const DETECT_WATCHDOG: Duration = Duration::from_secs(60);

/// Maximum watchdog re-issues per detection run.
const DETECT_MAX_RETRIES: u32 = 3;

/// Which tab is active.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ActiveTab {
    /// Cluster overview.
    Overview,
    /// One cluster tab (by name).
    Cluster(String),
}

/// A dismissable banner strip entry.
#[derive(Debug, Clone)]
pub struct Banner {
    /// Message text.
    pub text: String,
    /// Error (red) vs notice (blue).
    pub is_error: bool,
}

/// Dev/CI screenshot modes driven from the command line (documentation,
/// headless verification). The About window's manual capture is separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenshotMode {
    /// Capture once after the first rendered frame (and after `delay` from
    /// startup), save to this path, then close the app.
    Once {
        /// Destination PNG.
        path: std::path::PathBuf,
        /// Wait before capturing (lets async state like tool detection
        /// settle).
        delay: std::time::Duration,
    },
    /// Capture every N seconds into a directory (timestamped filenames).
    Every {
        /// Seconds between captures.
        interval: std::time::Duration,
        /// Destination directory (created on demand).
        dir: std::path::PathBuf,
    },
}

/// The main application state.
pub struct KindboardApp {
    /// UI → core.
    cmd: Sender<CoreCommand>,
    /// Core → UI.
    events: Receiver<CoreEvent>,
    /// Overview state (clusters + dependencies).
    overview: OverviewState,
    /// Open cluster tabs.
    tabs: Vec<ClusterTab>,
    /// Active tab.
    active: ActiveTab,
    /// Per-cluster operation panels (create/recreate/destroy).
    ops: HashMap<String, OpPanel>,
    /// Next-generation counters per cluster for ops.
    op_gens: HashMap<String, OpGen>,
    /// Create wizard.
    wizard: Option<WizardState>,
    /// Wizard visibility (open from overview).
    wizard_open: bool,
    /// About window open.
    about_open: bool,
    /// Dev/CI screenshot mode (None in normal use).
    screenshot: Option<ScreenshotMode>,
    /// The one-shot screenshot request was sent (waiting for the reply).
    screenshot_pending: bool,
    /// Frames rendered so far (used to capture after the first paint).
    frames: u64,
    /// App start time (used by the one-shot delay).
    started: std::time::Instant,
    /// Last periodic capture instant.
    last_capture: std::time::Instant,
    /// Global banners.
    banners: Vec<Banner>,
    /// Command bus is full (warning badge).
    bus_warning: bool,
    /// Dev/CI: open this cluster tab once the first reconcile lands
    /// (KINDBOARD_OPEN_CLUSTER=<name>).
    auto_open: Option<String>,
}

impl KindboardApp {
    /// Build the app, apply the theme, and kick off the initial probes.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cmd: Sender<CoreCommand>,
        events: Receiver<CoreEvent>,
        initial_theme: crate::theme::ThemeId,
    ) -> Self {
        theme::set_index(initial_theme);
        theme::apply(&cc.egui_ctx);
        // Dev/CI: KINDBOARD_OPEN_WIZARD=1 opens the create wizard on the
        // first frame so the screenshot harness can capture it headlessly.
        let wizard_open = std::env::var("KINDBOARD_OPEN_WIZARD").as_deref() == Ok("1");
        let wizard = wizard_open.then(wizard::WizardState::fresh);
        let mut app = KindboardApp {
            cmd,
            events,
            overview: OverviewState::default(),
            tabs: Vec::new(),
            active: ActiveTab::Overview,
            ops: HashMap::new(),
            op_gens: HashMap::new(),
            wizard,
            wizard_open,
            about_open: false,
            screenshot: None,
            screenshot_pending: false,
            frames: 0,
            started: std::time::Instant::now(),
            last_capture: std::time::Instant::now(),
            banners: Vec::new(),
            bus_warning: false,
            auto_open: std::env::var("KINDBOARD_OPEN_CLUSTER")
                .ok()
                .filter(|value| !value.is_empty()),
        };
        app.overview.loading = true;
        let detect_run = app.overview.deps.begin_detect();
        app.issue(CoreCommand::Reconcile);
        app.issue(CoreCommand::DetectTools { run: detect_run });
        app.issue(CoreCommand::CheckDockerDaemon);
        app
    }

    /// Enable a dev/CI screenshot mode (see [`ScreenshotMode`]).
    pub fn with_screenshot(mut self, mode: ScreenshotMode) -> Self {
        self.screenshot = Some(mode);
        self
    }

    /// Enqueue a command without ever blocking.
    fn issue(&mut self, command: CoreCommand) {
        match self.cmd.try_send(command) {
            Ok(()) => {
                self.bus_warning = false;
            }
            Err(TrySendError::Full(_)) => {
                // Core is busy (rare: the worker drains quickly); warn and
                // move on. The user can re-click.
                self.bus_warning = true;
            }
            Err(TrySendError::Disconnected(_)) => {
                self.banners.push(Banner {
                    text: "the core worker has stopped; restart the app".to_string(),
                    is_error: true,
                });
            }
        }
    }

    /// Next op generation for a cluster.
    fn next_op_gen(&mut self, name: &str) -> OpGen {
        let entry = self.op_gens.entry(name.to_string()).or_insert(0);
        *entry = entry.wrapping_add(1);
        *entry
    }

    /// Open (or focus) a cluster tab and start its first probes.
    fn open_tab(&mut self, name: &str) {
        if !self.tabs.iter().any(|tab| tab.name == name) {
            let record = self.overview.report.as_ref().and_then(|report| {
                report
                    .clusters
                    .iter()
                    .find(|entry| entry.name == name)
                    .and_then(|entry| match &entry.state {
                        ClusterState::Managed(record) | ClusterState::Missing(record) => {
                            Some(record.clone())
                        }
                        ClusterState::Adopted => None,
                    })
            });
            let mut tab = ClusterTab::new(name.to_string(), record);
            tab.manual_gen = 1;
            tab.topo_loading = true;
            self.issue(CoreCommand::CheckContext {
                name: name.to_string(),
            });
            self.issue(CoreCommand::FetchTopology {
                name: name.to_string(),
                op_gen: tab.manual_gen,
            });
            self.tabs.push(tab);
        }
        self.active = ActiveTab::Cluster(name.to_string());
    }

    /// Close a cluster tab: stop its log stream and auto-refresh.
    fn close_tab(&mut self, name: &str) {
        self.issue(CoreCommand::StopLogs {
            name: name.to_string(),
        });
        self.issue(CoreCommand::SetAutoRefresh {
            name: name.to_string(),
            interval_secs: 5,
            enabled: false,
            op_gen: 0,
        });
        self.tabs.retain(|tab| tab.name != name);
        if matches!(&self.active, ActiveTab::Cluster(active) if *active == name) {
            self.active = ActiveTab::Overview;
        }
    }

    /// Drain all pending events into state.
    fn drain_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.handle_event(event);
        }
    }

    /// Route one event.
    fn handle_event(&mut self, event: CoreEvent) {
        self.overview.deps.handle_event(&event);
        match event {
            CoreEvent::ReconcileDone { result } => {
                self.overview.loading = false;
                match *result {
                    Ok(report) => {
                        self.overview.report = Some(report);
                        self.overview.error = None;
                        // Refresh open tabs' records (a just-created or
                        // recreated cluster gets its authoritative spec).
                        if let Some(report) = &self.overview.report {
                            for tab in &mut self.tabs {
                                if let Some(entry) =
                                    report.clusters.iter().find(|entry| entry.name == tab.name)
                                    && let ClusterState::Managed(record) = &entry.state
                                {
                                    tab.record = Some(record.clone());
                                }
                            }
                        }
                        // Dev/CI: auto-open a cluster tab once live names
                        // are known (headless screenshot harness).
                        if let Some(name) = self.auto_open.take() {
                            self.open_tab(&name);
                        }
                    }
                    Err(err) => {
                        self.overview.error = Some(err);
                    }
                }
            }
            CoreEvent::LiveClusters { names } => {
                self.overview.live = names;
            }
            CoreEvent::ClusterStatus { name, status } => {
                self.overview.statuses.insert(name, status);
            }
            CoreEvent::DetectedTool { .. } | CoreEvent::ToolsDetectDone { .. } => {}
            CoreEvent::DockerDaemon { .. } => {}
            CoreEvent::InstallEvent { .. } => {}
            CoreEvent::InstallDone { .. } => {
                // Refresh every row with the post-install reality.
                let detect_run = self.overview.deps.begin_detect();
                self.issue(CoreCommand::DetectTools { run: detect_run });
            }
            CoreEvent::Provision {
                name,
                op_gen,
                event: provision_event,
            } => {
                if let Some(panel) = self.ops.get_mut(&name)
                    && panel.op_gen == op_gen
                {
                    panel.apply_event(&provision_event);
                }
            }
            CoreEvent::ProvisionDone {
                name,
                op_gen,
                result,
            } => {
                if let Some(panel) = self.ops.get_mut(&name)
                    && panel.op_gen == op_gen
                {
                    panel.finished = Some(result.clone());
                }
                if result.is_ok() {
                    let follow = self.ops.get(&name).map(|panel| panel.kind);
                    match follow {
                        Some(OpKind::Create) | Some(OpKind::Recreate) => {
                            self.issue(CoreCommand::Reconcile);
                            self.open_tab(&name);
                        }
                        Some(OpKind::Destroy) => {
                            self.issue(CoreCommand::Reconcile);
                            self.tabs.retain(|tab| tab.name != name);
                            if matches!(&self.active, ActiveTab::Cluster(active) if *active == name)
                            {
                                self.active = ActiveTab::Overview;
                            }
                        }
                        None => {}
                    }
                }
            }
            CoreEvent::ContextState { name, state } => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.name == name) {
                    tab.context_state = Some(state);
                }
            }
            CoreEvent::ContextEnsured { name, result } => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.name == name) {
                    tab.context_result = Some(result.clone());
                }
                match result {
                    Ok(()) => {
                        self.banners.push(Banner {
                            text: format!(
                                "kubeconfig context kind-{name} exported and set as current"
                            ),
                            is_error: false,
                        });
                    }
                    Err(err) => {
                        self.banners.push(Banner {
                            text: format!("kubeconfig export failed for {name}: {err}"),
                            is_error: true,
                        });
                    }
                }
            }
            CoreEvent::LogsExported { name, dest, result } => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.name == name) {
                    tab.last_export = Some((dest.clone(), result.clone()));
                }
                match result {
                    Ok(()) => {
                        self.banners.push(Banner {
                            text: format!("logs of {name} exported to {}", dest.display()),
                            is_error: false,
                        });
                    }
                    Err(err) => {
                        self.banners.push(Banner {
                            text: format!("log export failed for {name}: {err}"),
                            is_error: true,
                        });
                    }
                }
            }
            CoreEvent::Topology {
                name,
                op_gen,
                result,
            } => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.name == name)
                    && tab.accept_topology(op_gen)
                {
                    tab.apply_topology(*result);
                }
            }
            CoreEvent::LogLine { name, op_gen, line } => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.name == name)
                    && tab.accept_log(op_gen)
                {
                    tab.logs.ring.push(line);
                }
            }
            CoreEvent::LogEnded {
                name,
                op_gen,
                result,
            } => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.name == name)
                    && tab.accept_log(op_gen)
                {
                    tab.logs.running = false;
                    tab.logs.ended = Some(result);
                }
            }
            CoreEvent::BusFull => {
                self.banners.push(Banner {
                    text: "event bus overflow: some progress lines were dropped".to_string(),
                    is_error: true,
                });
            }
            CoreEvent::Error { context, message } => {
                self.banners.push(Banner {
                    text: format!("{context}: {message}"),
                    is_error: true,
                });
            }
            CoreEvent::Notice { message } => {
                self.banners.push(Banner {
                    text: message,
                    is_error: false,
                });
            }
        }
    }

    /// Handle global keyboard shortcuts.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if escape {
            if self.wizard_open {
                self.wizard_open = false;
                self.wizard = None;
            } else if self.about_open {
                self.about_open = false;
            } else if self.overview.destroy.is_some() {
                self.overview.destroy = None;
            } else if let ActiveTab::Cluster(name) = self.active.clone()
                && let Some(tab) = self.tabs.iter_mut().find(|tab| tab.name == name)
            {
                tab.confirm = None;
            }
        }
        let refresh = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::R));
        if refresh && let ActiveTab::Cluster(name) = self.active.clone() {
            let mut op_gen = 0;
            if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.name == name) {
                tab.manual_gen = tab.manual_gen.wrapping_add(1);
                tab.topo_loading = true;
                op_gen = tab.manual_gen;
            }
            if op_gen > 0 {
                self.issue(CoreCommand::FetchTopology { name, op_gen });
            }
        }
    }

    /// Poll for screenshot replies and save them.
    fn poll_screenshot(&mut self, ctx: &egui::Context) {
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = image else { return };
        match &self.screenshot {
            Some(ScreenshotMode::Once { path, .. }) => {
                match about::save_screenshot_to(&image, path) {
                    Ok(saved) => {
                        self.banners.push(Banner {
                            text: format!("screenshot saved to {}", saved.display()),
                            is_error: false,
                        });
                    }
                    Err(err) => {
                        // The app closes right after; make the failure
                        // visible on stderr too (CI consumes the log).
                        log::error!("screenshot failed: {err}");
                        self.banners.push(Banner {
                            text: format!("screenshot failed: {err}"),
                            is_error: true,
                        });
                    }
                }
                self.screenshot_pending = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(ScreenshotMode::Every { dir, .. }) => match about::screenshot_path(dir) {
                Some(path) => {
                    if let Err(err) = about::save_screenshot_to(&image, &path) {
                        log::error!("screenshot failed: {err}");
                        self.banners.push(Banner {
                            text: format!("screenshot failed: {err}"),
                            is_error: true,
                        });
                    }
                }
                None => {
                    log::error!("screenshot failed: could not create {}", dir.display());
                    self.banners.push(Banner {
                        text: format!("screenshot failed: could not create {}", dir.display()),
                        is_error: true,
                    });
                }
            },
            None => match about::save_screenshot(&image) {
                Ok(path) => {
                    self.banners.push(Banner {
                        text: format!("screenshot saved to {}", path.display()),
                        is_error: false,
                    });
                }
                Err(err) => {
                    self.banners.push(Banner {
                        text: format!("screenshot failed: {err}"),
                        is_error: true,
                    });
                }
            },
        }
    }

    /// Drive dev/CI screenshot modes: request captures per the mode's
    /// cadence (and keep the app repainting for periodic mode).
    fn drive_screenshot(&mut self, ctx: &egui::Context) {
        self.frames = self.frames.wrapping_add(1);
        match &self.screenshot {
            Some(ScreenshotMode::Once { delay, .. }) => {
                // Capture on the second frame (so the first frame has been
                // painted; the command snapshots at frame end) and after the
                // requested delay (so async state can settle). Keep
                // repainting until the capture fires — otherwise egui stops
                // rendering (no input) and a delayed capture never happens.
                if self.frames >= 2 && self.started.elapsed() >= *delay && !self.screenshot_pending
                {
                    self.screenshot_pending = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::UserData::default(),
                    ));
                }
                if !self.screenshot_pending {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
            Some(ScreenshotMode::Every { interval, .. }) => {
                if self.last_capture.elapsed() >= *interval {
                    self.last_capture = std::time::Instant::now();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                        egui::UserData::default(),
                    ));
                }
                ctx.request_repaint_after(*interval);
            }
            None => {}
        }
    }

    /// Whether any long-running activity needs periodic repaints.
    fn needs_tick(&self) -> bool {
        self.ops.values().any(OpPanel::running)
            || self
                .tabs
                .iter()
                .any(|tab| tab.logs.running || tab.auto.is_some() || tab.picker.is_some())
            || self
                .overview
                .deps
                .install
                .as_ref()
                .is_some_and(|install| install.done.is_none())
    }

    /// Top bar: brand, title, theme picker, bus warning, About.
    fn render_top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top(egui::Id::new("top-bar")).show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                crate::icons::brand_mark(ui, 26.0);
                ui.strong(egui::RichText::new("kindboard").size(16.0));
                ui.label(
                    egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .color(theme::pal().text_dim)
                        .size(11.0),
                );
                if self.bus_warning {
                    ui.separator();
                    ui.label(
                        egui::RichText::new("core busy - some requests may have been dropped")
                            .color(theme::pal().amber)
                            .size(11.0),
                    )
                    .on_hover_text("The command queue is full; retry the last action");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("About")
                        .on_hover_text("About kindboard, license and screenshot capture")
                        .clicked()
                    {
                        self.about_open = true;
                    }
                    let mut theme = theme::current();
                    egui::ComboBox::from_id_salt("theme-picker")
                        .selected_text(theme.label())
                        .show_ui(ui, |ui| {
                            for id in theme::ThemeId::ALL {
                                ui.selectable_value(&mut theme, id, id.label());
                            }
                        });
                    if theme != theme::current() {
                        theme::set_theme(ui.ctx(), theme);
                        self.issue(CoreCommand::SetTheme {
                            id: theme.id().to_string(),
                        });
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    /// Banner strip (errors + notices, dismissable).
    fn render_banners(&mut self, ui: &mut egui::Ui) {
        if self.banners.is_empty() {
            return;
        }
        let mut dismiss: Vec<usize> = Vec::new();
        egui::Panel::top(egui::Id::new("banners")).show(ui, |ui| {
            for (index, banner) in self.banners.iter().enumerate() {
                let color = if banner.is_error {
                    theme::pal().red
                } else {
                    theme::pal().accent
                };
                egui::Frame::new()
                    .fill(theme::pal().dim(color))
                    .stroke(egui::Stroke::new(1.0, color))
                    .corner_radius(egui::CornerRadius::same(4))
                    .inner_margin(egui::Margin::same(6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&banner.text).color(color).size(12.0));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Dismiss").clicked() {
                                        dismiss.push(index);
                                    }
                                },
                            );
                        });
                    });
                ui.add_space(2.0);
            }
        });
        for index in dismiss.into_iter().rev() {
            self.banners.remove(index);
        }
    }

    /// Tab bar: Overview + open cluster tabs (closable). Tabs scroll
    /// horizontally when the window is narrower than the tab strip.
    fn render_tab_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top(egui::Id::new("tab-bar")).show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("tabs")
                .auto_shrink([false, true])
                .max_height(40.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                let overview_selected = matches!(self.active, ActiveTab::Overview);
                if ui
                    .selectable_label(overview_selected, "Overview")
                    .on_hover_text("Clusters and dependencies")
                    .clicked()
                {
                    self.active = ActiveTab::Overview;
                }
                let mut close: Option<String> = None;
                for tab in &self.tabs {
                    let selected =
                        matches!(&self.active, ActiveTab::Cluster(name) if name == &tab.name);
                    ui.horizontal(|ui| {
                        // The cluster name must stay readable in both states:
                        // explicit high-contrast text on the selected tab.
                        let label = if selected {
                            RichText::new(format!("  {}  ", tab.name))
                                .strong()
                                .color(theme::pal().on_accent)
                        } else {
                            RichText::new(format!("  {}  ", tab.name))
                        };
                        if ui
                            .selectable_label(selected, label)
                            .on_hover_text("Open the cluster tab")
                            .clicked()
                        {
                            self.active = ActiveTab::Cluster(tab.name.clone());
                        }
                        let close_button = ui
                            .small_button("x")
                            .on_hover_text("Close this tab (stops logs and auto-refresh)");
                        if close_button.clicked() {
                            close = Some(tab.name.clone());
                        }
                    });
                }
                if let Some(name) = close {
                    self.close_tab(&name);
                }
                    });
                });
        });
    }

    /// Render the active view and execute its commands.
    fn render_active(&mut self, ui: &mut egui::Ui) {
        let s = &mut *self;
        let mut overview_cmds: Vec<OverviewCmd> = Vec::new();
        let mut tab_cmds: Vec<TabCmd> = Vec::new();
        match s.active {
            ActiveTab::Overview => {
                egui::CentralPanel::default().show(ui, |ui| {
                    // Dependencies live in a right panel (contracts UI
                    // scope).
                    egui::Panel::right(egui::Id::new("overview-deps"))
                        .resizable(true)
                        .default_size(320.0)
                        .min_size(240.0)
                        .max_size(480.0)
                        .show(ui, |ui| {
                            let mut actions = Vec::new();
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    crate::views::deps::show(
                                        ui,
                                        &mut s.overview.deps,
                                        &mut actions,
                                    );
                                });
                            for action in actions {
                                overview_cmds.push(OverviewCmd::Command(action));
                            }
                        });
                    egui::CentralPanel::default().show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let mut open_wizard = s.wizard_open;
                                overview_cmds.extend(overview::show(
                                    ui,
                                    &mut s.overview,
                                    &mut open_wizard,
                                ));
                                s.wizard_open = open_wizard;
                            });
                    });
                });
            }
            ActiveTab::Cluster(ref name) => {
                let Some(index) = s.tabs.iter().position(|tab| &tab.name == name) else {
                    return;
                };
                egui::CentralPanel::default().show(ui, |ui| {
                    tab_cmds.extend(cluster::show(ui, &mut s.tabs[index]));
                });
            }
        }

        for cmd in overview_cmds {
            match cmd {
                OverviewCmd::Command(command) => self.issue(command),
                OverviewCmd::Destroy { name, record_only } => {
                    let op_gen = self.next_op_gen(&name);
                    self.ops.insert(
                        name.clone(),
                        OpPanel::start(OpKind::Destroy, name.clone(), op_gen),
                    );
                    let _ = record_only; // core tolerates a missing cluster
                    self.issue(CoreCommand::DestroyCluster { name, op_gen });
                }
                OverviewCmd::Recreate { name } => {
                    let spec = self
                        .overview
                        .report
                        .as_ref()
                        .and_then(|report| report.clusters.iter().find(|entry| entry.name == name))
                        .and_then(|entry| match &entry.state {
                            ClusterState::Missing(record) => Some(record.spec.clone()),
                            _ => None,
                        });
                    if let Some(spec) = spec {
                        let op_gen = self.next_op_gen(&name);
                        self.ops.insert(
                            name.clone(),
                            OpPanel::start(OpKind::Recreate, name.clone(), op_gen),
                        );
                        self.issue(CoreCommand::RecreateCluster {
                            name,
                            spec: Box::new(spec),
                            op_gen,
                        });
                    }
                }
                OverviewCmd::Scale { name, workers } => {
                    // Guided recreate from the stored spec with a new
                    // worker count (the card's node-count control).
                    let spec = self
                        .overview
                        .report
                        .as_ref()
                        .and_then(|report| report.clusters.iter().find(|entry| entry.name == name))
                        .and_then(|entry| match &entry.state {
                            ClusterState::Managed(record) => {
                                let mut spec = record.spec.clone();
                                spec.worker_count = workers;
                                Some(spec)
                            }
                            _ => None,
                        });
                    if let Some(spec) = spec {
                        let op_gen = self.next_op_gen(&name);
                        self.ops.insert(
                            name.clone(),
                            OpPanel::start(OpKind::Recreate, name.clone(), op_gen),
                        );
                        self.issue(CoreCommand::RecreateCluster {
                            name,
                            spec: Box::new(spec),
                            op_gen,
                        });
                    }
                }
                OverviewCmd::OpenCluster(name) => self.open_tab(&name),
            }
        }
        for cmd in tab_cmds {
            match cmd {
                TabCmd::Command(command) => self.issue(command),
                TabCmd::Destroy { name } => {
                    let op_gen = self.next_op_gen(&name);
                    self.ops.insert(
                        name.clone(),
                        OpPanel::start(OpKind::Destroy, name.clone(), op_gen),
                    );
                    self.issue(CoreCommand::DestroyCluster { name, op_gen });
                }
                TabCmd::Recreate { spec } => {
                    let name = spec.name.clone();
                    let op_gen = self.next_op_gen(&name);
                    self.ops.insert(
                        name.clone(),
                        OpPanel::start(OpKind::Recreate, name.clone(), op_gen),
                    );
                    self.issue(CoreCommand::RecreateCluster { name, spec, op_gen });
                }
                TabCmd::Close => {
                    let name = match &self.active {
                        ActiveTab::Cluster(name) => name.clone(),
                        ActiveTab::Overview => continue,
                    };
                    self.close_tab(&name);
                }
                TabCmd::RefreshOverview => {
                    self.issue(CoreCommand::Reconcile);
                }
            }
        }
    }

    /// Render operation panels and collect close requests.
    fn render_op_panels(&mut self, ctx: &egui::Context) {
        let mut remove: Vec<String> = Vec::new();
        let mut pending: Vec<CoreCommand> = Vec::new();
        for (name, panel) in &mut self.ops {
            let mut actions = Vec::new();
            if crate::views::ops::show(ctx, panel, &mut actions) {
                remove.push(name.clone());
            }
            pending.extend(actions);
        }
        for action in pending {
            self.issue(action);
        }
        for name in remove {
            self.ops.remove(&name);
        }
    }

    /// Render the create wizard modal.
    fn render_wizard(&mut self, ctx: &egui::Context) {
        if !self.wizard_open {
            self.wizard = None;
            return;
        }
        if self.wizard.is_none() {
            self.wizard = Some(WizardState::fresh());
        }
        let Some(state) = self.wizard.as_mut() else {
            return;
        };
        match wizard::show(ctx, state) {
            WizardAction::Stay => {}
            WizardAction::Cancel => {
                self.wizard_open = false;
                self.wizard = None;
            }
            WizardAction::Create(spec) => {
                let name = spec.name.clone();
                let op_gen = self.next_op_gen(&name);
                self.ops.insert(
                    name.clone(),
                    OpPanel::start(OpKind::Create, name.clone(), op_gen),
                );
                self.issue(CoreCommand::CreateCluster {
                    spec: Box::new(spec),
                    op_gen,
                });
                self.wizard_open = false;
                self.wizard = None;
            }
        }
    }

    /// Render the About window + screenshot button handling.
    fn render_about(&mut self, ctx: &egui::Context) {
        if self.about_open && about::show(ctx, &mut self.about_open) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
    }

    /// Self-healing for tool detection: the worker's detect run can stall
    /// (rare race); re-issue it after a stall threshold, bounded, and keep
    /// the UI repainting while a run is in flight so the watchdog actually
    /// ticks.
    fn watch_detect(&mut self, ctx: &egui::Context) {
        if self.overview.deps.detecting {
            ctx.request_repaint_after(DETECT_TICK);
            if self.overview.deps.detect_stalled(DETECT_WATCHDOG) {
                if self.overview.deps.detect_retries < DETECT_MAX_RETRIES {
                    self.overview.deps.detect_retries += 1;
                    let detect_run = self.overview.deps.begin_detect();
                    log::warn!(
                        "tool detection stalled; re-running (attempt {}/{})",
                        self.overview.deps.detect_retries,
                        DETECT_MAX_RETRIES
                    );
                    self.issue(CoreCommand::DetectTools { run: detect_run });
                } else {
                    // Budget exhausted: release the UI instead of leaving
                    // Refresh disabled forever — the user can retry
                    // manually at any time (TRACE-013).
                    log::error!("tool detection stalled repeatedly; releasing the UI");
                    self.overview.deps.detecting = false;
                    self.overview.deps.detect_started = None;
                    self.overview.deps.detect_retries = 0;
                }
            }
        }
    }
}

impl eframe::App for KindboardApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.handle_shortcuts(ctx);
        self.drive_screenshot(ctx);
        self.poll_screenshot(ctx);
        self.watch_detect(ctx);
        if self.needs_tick() {
            ctx.request_repaint_after(ACTIVE_TICK);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // The root Ui has no background or margin; a central panel provides
        // both, and the chrome panels are shown inside it.
        egui::CentralPanel::default_margins().show(ui, |ui| {
            self.render_top_bar(ui);
            self.render_banners(ui);
            self.render_tab_bar(ui);
            self.render_active(ui);
        });
        self.render_op_panels(&ctx);
        self.render_wizard(&ctx);
        self.render_about(&ctx);
    }
}
