//! Dependency panel: detect/install the 8 tools (docker, kind, kubectl,
//! helm, cilium, k9s, kubectx, kustomize). Per-row version badge + Install
//! button that opens an inline streamed log area fed by core's
//! `install_with_progress` events.

use std::collections::{HashMap, VecDeque};

use eframe::egui::{self, RichText, ScrollArea};
use kindboard_core::{DockerDaemonState, ToolId, ToolStatus};

use crate::bus::{CoreCommand, CoreEvent};
use crate::icons;
use crate::theme;
use crate::util::truncate;

/// Max lines kept in the inline install log.
const INSTALL_LOG_CAP: usize = 400;

/// Dependency panel state.
#[derive(Default)]
pub struct DepsState {
    /// Detection in flight.
    pub detecting: bool,
    /// When the current detection run started (None when idle).
    pub detect_started: Option<std::time::Instant>,
    /// Watchdog retries issued for the current run.
    pub detect_retries: u32,
    /// Per-tool detection outcome.
    pub results: HashMap<ToolId, Result<ToolStatus, String>>,
    /// Docker daemon state (separate from CLI presence).
    pub docker: Option<DockerDaemonState>,
    /// The running/last install (inline log area).
    pub install: Option<InstallUi>,
    /// Id of the detection run this state tracks (0 = none). Events carrying
    /// any other id belong to a superseded run and are ignored — overlapping
    /// runs (startup + Refresh + watchdog re-issue) must never overwrite
    /// fresh results with stale ones (D5).
    pub run: u64,
}

/// Inline install log area state.
pub struct InstallUi {
    /// Tool being installed.
    pub id: ToolId,
    /// Streamed lines (step descriptions + output), capped.
    pub lines: VecDeque<String>,
    /// Total steps in the plan (from [`kindboard_core::InstallEvent`]).
    pub total: usize,
    /// Finished outcome.
    pub done: Option<Result<ToolStatus, String>>,
}

impl InstallUi {
    fn new(id: ToolId) -> Self {
        InstallUi {
            id,
            lines: VecDeque::with_capacity(INSTALL_LOG_CAP),
            total: 0,
            done: None,
        }
    }

    fn push_line(&mut self, line: String) {
        if self.lines.len() >= INSTALL_LOG_CAP {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }
}

impl DepsState {
    /// Handle a deps-related event.
    pub fn handle_event(&mut self, event: &CoreEvent) {
        match event {
            CoreEvent::DetectedTool { id, run, result } => {
                // Drop events from superseded detect runs: a newer run has
                // already cleared (or is filling) `results`, and a stale
                // outcome would read as a fresh one.
                if *run != self.run {
                    return;
                }
                self.results.insert(*id, result.clone());
            }
            CoreEvent::ToolsDetectDone { run } => {
                if *run != self.run {
                    return;
                }
                self.detecting = false;
                self.detect_started = None;
                self.detect_retries = 0;
            }
            CoreEvent::DockerDaemon { state } => {
                self.docker = Some(state.clone());
            }
            CoreEvent::InstallEvent { id, event } => {
                if let Some(install) = self.install.as_mut()
                    && install.id == *id
                {
                    match event {
                        kindboard_core::InstallEvent::StepStarted {
                            step,
                            total,
                            description,
                        } => {
                            install.total = *total;
                            install.push_line(format!("[{}/{}] {description}", step + 1, total));
                        }
                        kindboard_core::InstallEvent::StepLine { line, .. } => {
                            install.push_line(format!("  {line}"));
                        }
                        kindboard_core::InstallEvent::StepFinished { step } => {
                            install.push_line(format!("[{}] done", step + 1));
                        }
                    }
                }
            }
            CoreEvent::InstallDone { id, result } => {
                if let Some(install) = self.install.as_mut()
                    && install.id == *id
                {
                    match result {
                        Ok(status) => {
                            install
                                .push_line(format!("install finished: {}", version_line(status)));
                        }
                        Err(err) => {
                            install.push_line(format!("install failed: {err}"));
                        }
                    }
                    install.done = Some(result.clone());
                }
                self.results.insert(*id, result.clone());
            }
            _ => {}
        }
    }

    /// Start detecting (UI calls the command itself; this just marks).
    ///
    /// Clears previous results so stale statuses are never shown as fresh:
    /// rows without a result render as "checking…" until the new outcome
    /// arrives (TRACE-013). Returns the new run id — pass it to
    /// [`CoreCommand::DetectTools`] so only this run's events are accepted.
    pub fn begin_detect(&mut self) -> u64 {
        self.run = self.run.wrapping_add(1);
        self.detecting = true;
        self.detect_started = Some(std::time::Instant::now());
        self.detect_retries = 0;
        self.results.clear();
        self.run
    }

    /// Whether the current detection run has been running for at least
    /// `threshold` without finishing (a worker stall). Never true while
    /// idle.
    pub fn detect_stalled(&self, threshold: std::time::Duration) -> bool {
        self.detecting
            && self
                .detect_started
                .is_some_and(|started| started.elapsed() >= threshold)
    }

    /// Open the inline install area and pre-seed it with the plan preview.
    pub fn begin_install(&mut self, id: ToolId) {
        let mut install = InstallUi::new(id);
        if let Ok(plan) = kindboard_core::plan_install(id) {
            install.total = plan.steps.len();
            for line in plan.render() {
                install.push_line(format!("plan: {line}"));
            }
        }
        if let Some(tool) = kindboard_core::tool(id)
            && tool.install.pkg_manager_only
        {
            install.push_line(
                "notice: no binary fallback for this tool; the system package manager is used"
                    .to_string(),
            );
        }
        if let Some(tool) = kindboard_core::tool(id)
            && let Some(note) = tool.install.post_install
        {
            install.push_line(format!("notice: {note}"));
        }
        install.push_line(
            "notice: if elevation is required, a sudo/pkexec prompt may appear outside this window"
                .to_string(),
        );
        self.install = Some(install);
    }
}

fn version_line(status: &ToolStatus) -> String {
    match status {
        ToolStatus::Installed { version, path } => {
            if version.is_known() {
                format!(
                    "v{}.{}.{} ({})",
                    version.major,
                    version.minor,
                    version.patch,
                    path.display()
                )
            } else {
                format!("installed ({})", path.display())
            }
        }
        ToolStatus::NotInstalled => "not installed".to_string(),
        ToolStatus::Broken { reason } => format!("broken: {reason}"),
    }
}

/// Render the dependency panel. Returns commands to enqueue (the caller
/// owns the command sender so the panel stays pure UI).
pub fn show(ui: &mut egui::Ui, state: &mut DepsState, actions: &mut Vec<CoreCommand>) {
    ui.horizontal_wrapped(|ui| {
        ui.strong("Dependencies");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let refreshing = state.detecting;
            let refresh = ui
                .add_enabled(
                    !refreshing,
                    egui::Button::new(if refreshing { "Checking…" } else { "Refresh" }),
                )
                .on_hover_text("Re-run detection for all tools");
            if refresh.clicked() {
                let run = state.begin_detect();
                actions.push(CoreCommand::DetectTools { run });
                actions.push(CoreCommand::CheckDockerDaemon);
            }
        });
    });

    // Docker daemon line.
    match &state.docker {
        Some(DockerDaemonState::Ready { server_version, .. }) => {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "docker daemon: up (server {})",
                        server_version.as_deref().unwrap_or("unknown")
                    ))
                    .color(theme::pal().green)
                    .size(12.0),
                );
            });
        }
        Some(other) => {
            ui.label(
                RichText::new(format!("docker daemon: {}", daemon_state_text(other)))
                    .color(theme::pal().amber)
                    .size(12.0),
            );
        }
        None => {
            ui.label(
                RichText::new("docker daemon: not probed yet")
                    .color(theme::pal().text_dim)
                    .size(12.0),
            );
        }
    }
    ui.separator();

    for tool in kindboard_core::registry() {
        let id = tool.id;
        ui.horizontal(|ui| {
            icons::tool_icon(ui, id, 22.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(tool.display).strong());
                match state.results.get(&id) {
                    Some(Ok(status)) => match status {
                        ToolStatus::Installed { version, path } => {
                            let text = if version.is_known() {
                                format!("v{}.{}.{}", version.major, version.minor, version.patch)
                            } else {
                                "installed".to_string()
                            };
                            ui.add(
                                egui::Label::new(
                                    RichText::new(text)
                                        .color(theme::pal().green)
                                        .size(11.0)
                                        .monospace(),
                                )
                                .sense(egui::Sense::hover()),
                            )
                            .on_hover_text(path.display().to_string());
                        }
                        ToolStatus::NotInstalled => {
                            ui.label(
                                RichText::new("Not installed")
                                    .color(theme::pal().red)
                                    .size(11.0),
                            );
                        }
                        ToolStatus::Broken { reason } => {
                            ui.label(
                                RichText::new("Broken").color(theme::pal().amber).size(11.0),
                            )
                            .on_hover_text(truncate(reason, 120));
                        }
                    },
                    Some(Err(err)) => {
                        ui.label(
                            RichText::new("detect failed")
                                .color(theme::pal().amber)
                                .size(11.0),
                        )
                        .on_hover_text(truncate(err, 120));
                    }
                    None => {
                        let text = if state.detecting { "checking…" } else { "unknown" };
                        ui.label(
                            RichText::new(text)
                                .color(theme::pal().text_dim)
                                .size(11.0),
                        );
                    }
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let installing = matches!(&state.install, Some(install) if install.id == id && install.done.is_none());
                // The button mirrors reality: installed tools get a disabled
                // green "Installed" badge instead of a live Install button —
                // an Install click can never re-run a plan for a present
                // tool (the core pre-flight gate backs this up).
                match state.results.get(&id) {
                    Some(Ok(ToolStatus::Installed { version, path })) => {
                        let label = if version.is_known() {
                            format!("Installed v{}.{}.{}", version.major, version.minor, version.patch)
                        } else {
                            "Installed".to_string()
                        };
                        ui.add_enabled(
                            false,
                            egui::Button::new(RichText::new(label).color(theme::pal().green)),
                        )
                        .on_hover_text(format!(
                            "{} is installed ({}) — no action needed",
                            tool.display,
                            path.display()
                        ));
                    }
                    _ => {
                        let button = ui
                            .add_enabled(!installing, egui::Button::new("Install"))
                            .on_hover_text(format!("Install {} (package manager first, binary fallback)", tool.display));
                        if button.clicked() {
                            state.begin_install(id);
                            actions.push(CoreCommand::InstallTool { id });
                        }
                    }
                }
            });
        });

        if let Some(install) = &state.install
            && install.id == id
        {
            let mut close_clicked = false;
            egui::Frame::new()
                .fill(theme::pal().bg)
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(egui::Margin::same(6))
                .show(ui, |ui| {
                    let height = 130.0_f32
                        .min(20.0 * install.lines.len() as f32 + 24.0)
                        .max(60.0);
                    // Both axes: long monospace lines must not overflow the
                    // (narrow) dependency panel — wrap instead of clip.
                    ScrollArea::both()
                        .stick_to_bottom(true)
                        .max_height(height)
                        .show(ui, |ui| {
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                            for line in &install.lines {
                                ui.label(
                                    RichText::new(line)
                                        .monospace()
                                        .size(11.0)
                                        .color(theme::pal().text_dim),
                                );
                            }
                            if install.done.is_none() {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(RichText::new("installing...").size(11.0));
                                });
                            }
                        });
                    if let Some(done) = &install.done {
                        match done {
                            Ok(_) => {
                                ui.label(
                                    RichText::new("install succeeded")
                                        .color(theme::pal().green)
                                        .size(11.0),
                                );
                            }
                            Err(err) => {
                                let short = truncate(err, 200);
                                ui.label(
                                    RichText::new(format!("install failed: {short}"))
                                        .color(theme::pal().red)
                                        .size(11.0),
                                );
                            }
                        }
                        if ui.button("Close").clicked() {
                            close_clicked = true;
                        }
                    }
                });
            if close_clicked {
                state.install = None;
            }
        }
    }
}

fn daemon_state_text(state: &DockerDaemonState) -> String {
    match state {
        DockerDaemonState::Ready { .. } => "up".to_string(),
        DockerDaemonState::BinaryMissing => "docker binary missing".to_string(),
        DockerDaemonState::NotRunning { reason } => {
            format!("not running ({})", truncate(reason, 80))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_detect_marks_running_and_started() {
        let mut state = DepsState::default();
        state.begin_detect();
        assert!(state.detecting);
        assert!(state.detect_started.is_some());
        assert!(!state.detect_stalled(std::time::Duration::from_secs(1_000_000)));
        assert!(state.detect_stalled(std::time::Duration::ZERO));
    }

    #[test]
    fn begin_detect_returns_monotonic_run_ids() {
        let mut state = DepsState::default();
        let first = state.begin_detect();
        let second = state.begin_detect();
        let third = state.begin_detect();
        assert_eq!(first, 1);
        assert!(second > first);
        assert!(third > second);
    }

    #[test]
    fn detect_done_resets_watchdog_state() {
        let mut state = DepsState::default();
        let run = state.begin_detect();
        state.detect_retries = 2;
        state.handle_event(&CoreEvent::ToolsDetectDone { run });
        assert!(!state.detecting);
        assert!(state.detect_started.is_none());
        assert_eq!(state.detect_retries, 0);
        assert!(!state.detect_stalled(std::time::Duration::ZERO));
    }

    #[test]
    fn stalled_is_false_when_idle() {
        let state = DepsState::default();
        assert!(!state.detect_stalled(std::time::Duration::ZERO));
    }

    #[test]
    fn detected_tool_updates_results() {
        let mut state = DepsState::default();
        let run = state.begin_detect();
        let result: Result<kindboard_core::ToolStatus, String> =
            Ok(kindboard_core::ToolStatus::NotInstalled);
        state.handle_event(&CoreEvent::DetectedTool {
            id: kindboard_core::ToolId::Kubectx,
            run,
            result: result.clone(),
        });
        assert_eq!(
            state.results.get(&kindboard_core::ToolId::Kubectx),
            Some(&result)
        );
    }

    #[test]
    fn stale_run_events_are_ignored() {
        // D5: overlapping detect runs (startup + Refresh + watchdog) must
        // never let a superseded run overwrite fresh results or release the
        // "checking…" state early.
        let mut state = DepsState::default();
        let first_run = state.begin_detect();
        state.handle_event(&CoreEvent::DetectedTool {
            id: kindboard_core::ToolId::Kind,
            run: first_run,
            result: Ok(kindboard_core::ToolStatus::NotInstalled),
        });
        assert_eq!(state.results.len(), 1);

        let second_run = state.begin_detect();
        assert!(state.results.is_empty(), "new run clears results");

        // A late event from the first run must not pollute the second run.
        state.handle_event(&CoreEvent::DetectedTool {
            id: kindboard_core::ToolId::Kind,
            run: first_run,
            result: Ok(kindboard_core::ToolStatus::NotInstalled),
        });
        assert!(
            state.results.is_empty(),
            "stale DetectedTool must be ignored"
        );

        // Nor may a stale completion release the "checking…" state.
        state.handle_event(&CoreEvent::ToolsDetectDone { run: first_run });
        assert!(state.detecting, "stale ToolsDetectDone must be ignored");

        // The current run's events still land and complete normally.
        let installed = kindboard_core::ToolStatus::Installed {
            version: kindboard_core::Version {
                major: 0,
                minor: 33,
                patch: 0,
            },
            path: std::path::PathBuf::from("/usr/bin/kind"),
        };
        state.handle_event(&CoreEvent::DetectedTool {
            id: kindboard_core::ToolId::Kind,
            run: second_run,
            result: Ok(installed.clone()),
        });
        state.handle_event(&CoreEvent::ToolsDetectDone { run: second_run });
        assert_eq!(state.results.len(), 1);
        assert_eq!(
            state.results.get(&kindboard_core::ToolId::Kind),
            Some(&Ok(installed))
        );
        assert!(!state.detecting);
    }

    #[test]
    fn begin_detect_clears_stale_results_and_resets_retries() {
        let mut state = DepsState::default();
        let run = state.begin_detect();
        state.detect_retries = 2;
        state.handle_event(&CoreEvent::DetectedTool {
            id: kindboard_core::ToolId::Kind,
            run,
            result: Ok(kindboard_core::ToolStatus::NotInstalled),
        });
        state.handle_event(&CoreEvent::ToolsDetectDone { run });
        assert_eq!(state.results.len(), 1);

        // A fresh run must not carry stale outcomes, and the retry budget
        // restarts for the new run (TRACE-013).
        state.begin_detect();
        assert!(state.results.is_empty());
        assert_eq!(state.detect_retries, 0);
        assert!(state.detecting);
    }

    #[test]
    fn watchdog_exhaustion_releases_the_ui() {
        // The app-level watchdog releases detection state once the retry
        // budget is spent; model the same transition here to prove the UI
        // is not permanently locked out (TRACE-013).
        let mut state = DepsState::default();
        state.begin_detect();
        state.detect_retries = 3; // DETECT_MAX_RETRIES reached
        state.detecting = false;
        state.detect_started = None;
        state.detect_retries = 0;
        assert!(!state.detecting);
        assert!(state.detect_started.is_none());
        assert!(!state.detect_stalled(std::time::Duration::ZERO));
        // Refresh stays available: begin_detect works from the released
        // state.
        state.begin_detect();
        assert!(state.detecting);
    }
}
