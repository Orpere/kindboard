//! Per-cluster tab: header (name/status/version/kubeconfig context state),
//! node list, the topology diagram with toolbar, the detail side panel,
//! the logs sub-view, and the destructive-action confirmations.
//!
//! Generation discipline (see `bus.rs`):
//! - manual topology snapshots carry `manual_gen`; auto-refresh carries its
//!   own `auto.op_gen`; [`ClusterTab::accept_topology`] accepts an event when
//!   its op_gen matches *either* active generation.
//! - log streams carry `logs.op_gen`; lines/endings from replaced streams are
//!   dropped.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use eframe::egui::{self, Modal, RichText, ScrollArea};
use kindboard_core::{ClusterRecord, KubeconfigStore, LogRing, LogSource, NodeRole, TopologyGraph};

use crate::bus::{ContextStatus, CoreCommand, OpGen};
use crate::theme;
use crate::util::{pill, status_dot};
use crate::views::diagram::{self, DiagramState};

/// Per-cluster tab state.
pub struct ClusterTab {
    /// Cluster name (tab identity).
    pub name: String,
    /// Stored record (None for adopted clusters).
    pub record: Option<ClusterRecord>,
    /// Latest topology snapshot.
    pub topo: Option<TopologyGraph>,
    /// Last topology failure (shown as an error banner).
    pub topo_error: Option<String>,
    /// A snapshot fetch is in flight.
    pub topo_loading: bool,
    /// Generation of the latest manual fetch.
    pub manual_gen: OpGen,
    /// Auto-refresh state (`Some` = enabled).
    pub auto: Option<AutoRefresh>,
    /// Diagram pan/zoom.
    pub diagram: DiagramState,
    /// Collapsed namespaces (their members are hidden).
    pub collapsed: HashSet<String>,
    /// Selected node id (detail panel).
    pub selected: Option<String>,
    /// Kubeconfig context state (checked on tab open).
    pub context_state: Option<ContextStatus>,
    /// Result of the last "Export kubeconfig" action.
    pub context_result: Option<Result<(), String>>,
    /// Log sub-view.
    pub logs: LogsUi,
    /// Open confirmation dialog.
    pub confirm: Option<TabConfirm>,
    /// Scale-workers stepper value.
    pub workers: u32,
    /// Directory picker in flight (export logs).
    pub picker: Option<Receiver<Option<PathBuf>>>,
    /// Last export-logs outcome.
    pub last_export: Option<(PathBuf, Result<(), String>)>,
}

/// Auto-refresh state.
pub struct AutoRefresh {
    /// Generation of the auto task's events.
    pub op_gen: OpGen,
    /// Interval in seconds.
    pub interval_secs: u64,
}

/// Confirmation dialogs inside a tab.
pub enum TabConfirm {
    /// Destroy: typed name must match.
    Destroy { typed: String },
    /// Recreate (scale): typed name must match.
    Recreate { typed: String },
    /// Delete a worker node (guided recreate with one fewer worker).
    DeleteNode {
        /// Node to remove.
        node: String,
        /// Typed cluster name must match.
        typed: String,
    },
}

/// Which log source the picker is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogSourceKind {
    /// Node container via docker logs.
    Docker { container: String },
    /// Pod via kubectl logs.
    Pod { ns: String, pod: String },
}

/// Log sub-view state.
pub struct LogsUi {
    /// Bounded display ring (same type core streams into).
    pub ring: LogRing,
    /// Generation of the active stream.
    pub op_gen: OpGen,
    /// A stream is running.
    pub running: bool,
    /// Follow mode.
    pub follow: bool,
    /// Tail length for one-shot reads.
    pub tail: u32,
    /// Selected source kind.
    pub source: LogSourceKind,
    /// Last stream outcome.
    pub ended: Option<Result<(), String>>,
}

impl LogsUi {
    fn new(cluster: &str) -> Self {
        LogsUi {
            ring: LogRing::new(kindboard_core::k8s::DEFAULT_LOG_RING_CAP),
            op_gen: 0,
            running: false,
            follow: true,
            tail: 100,
            source: LogSourceKind::Docker {
                container: format!("{cluster}-control-plane"),
            },
            ended: None,
        }
    }

    /// Convert the picked source into a core [`LogSource`].
    fn to_core_source(&self, cluster: &str) -> LogSource {
        match &self.source {
            LogSourceKind::Docker { container } => LogSource::Docker {
                container: container.clone(),
            },
            LogSourceKind::Pod { ns, pod } => LogSource::Kubectl {
                context: KubeconfigStore::kind_context_name(cluster),
                pod: pod.clone(),
                ns: ns.clone(),
                container: None,
            },
        }
    }
}

impl ClusterTab {
    /// Open a tab for a cluster.
    pub fn new(name: String, record: Option<ClusterRecord>) -> Self {
        let workers = record
            .as_ref()
            .map(|record| record.spec.worker_count)
            .unwrap_or(0);
        ClusterTab {
            logs: LogsUi::new(&name),
            name,
            record,
            topo: None,
            topo_error: None,
            topo_loading: false,
            manual_gen: 0,
            auto: None,
            diagram: DiagramState::new(),
            collapsed: HashSet::new(),
            selected: None,
            context_state: None,
            context_result: None,
            confirm: None,
            workers,
            picker: None,
            last_export: None,
        }
    }

    /// Whether a topology event with this generation belongs to an active
    /// request of this tab.
    pub fn accept_topology(&self, op_gen: OpGen) -> bool {
        op_gen == self.manual_gen || self.auto.as_ref().is_some_and(|auto| auto.op_gen == op_gen)
    }

    /// Apply a topology result.
    pub fn apply_topology(&mut self, result: Result<TopologyGraph, String>) {
        match result {
            Ok(graph) => {
                // Namespaces present in the graph stay visible; namespaces
                // that disappeared leave the collapsed set (harmless).
                self.collapsed
                    .retain(|name| graph.namespaces.iter().any(|ns| &ns.name == name));
                self.topo_error = None;
                // Refit only on the first successful snapshot; later polls
                // keep the user's pan/zoom.
                if self.topo.is_none() {
                    self.diagram.fit_next = true;
                }
                self.topo = Some(graph);
            }
            Err(err) => {
                self.topo_error = Some(err);
            }
        }
        self.topo_loading = false;
    }

    /// Whether a log event with this generation belongs to the active
    /// stream.
    pub fn accept_log(&self, op_gen: OpGen) -> bool {
        op_gen == self.logs.op_gen
    }

    /// Record of the cluster, or `None` for adopted clusters.
    pub fn has_record(&self) -> bool {
        self.record.is_some()
    }

    /// The spec with a new worker count (for the recreate confirm).
    pub fn spec_with_workers(&self) -> Option<kindboard_core::ClusterSpec> {
        self.record.as_ref().map(|record| {
            let mut spec = record.spec.clone();
            spec.worker_count = self.workers;
            spec
        })
    }
}

/// Actions a tab wants the app to perform.
pub enum TabCmd {
    /// Enqueue a command (op_gen already assigned).
    Command(CoreCommand),
    /// Destroy the cluster (app assigns the op generation + opens the
    /// panel).
    Destroy { name: String },
    /// Recreate the cluster with this spec (app assigns the generation).
    Recreate {
        spec: Box<kindboard_core::ClusterSpec>,
    },
    /// Close this tab.
    Close,
    /// Refresh the overview (after destructive ops).
    RefreshOverview,
}

/// Render the tab. Returns commands to execute.
#[allow(clippy::too_many_lines)] // one tab = one cohesive screen; helpers
// below keep each section readable.
pub fn show(ui: &mut egui::Ui, tab: &mut ClusterTab) -> Vec<TabCmd> {
    let mut cmds: Vec<TabCmd> = Vec::new();

    // ---- header ----------------------------------------------------------
    egui::Panel::top(egui::Id::new(("cluster-header", tab.name.clone()))).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.strong(RichText::new(&tab.name).size(16.0));
            match &tab.topo {
                Some(_) => pill(ui, "running", theme::GREEN),
                None => pill(ui, "topology unknown", theme::GREY),
            }
            if let Some(record) = &tab.record {
                ui.label(
                    RichText::new(format!("k8s {}", record.spec.k8s_version.as_str()))
                        .color(theme::TEXT_DIM),
                );
                ui.label(
                    RichText::new(format!("{:?} CNI", record.spec.cni)).color(theme::TEXT_DIM),
                );
            } else {
                pill(ui, "adopted", theme::GREY);
            }
            // Kubeconfig context state.
            match tab.context_state {
                Some(ContextStatus::Current) => {
                    pill(ui, "context current", theme::GREEN);
                }
                Some(ContextStatus::Present) => {
                    pill(ui, "context present", theme::AMBER);
                }
                Some(ContextStatus::Absent) => {
                    pill(ui, "context absent", theme::RED);
                }
                None => {
                    pill(ui, "context unknown", theme::GREY);
                }
            }
            if let Some(result) = &tab.context_result {
                match result {
                    Ok(()) => {
                        ui.label(
                            RichText::new("kubeconfig exported")
                                .color(theme::GREEN)
                                .size(11.0),
                        );
                    }
                    Err(err) => {
                        ui.label(
                            RichText::new(format!("export failed: {err}"))
                                .color(theme::RED)
                                .size(11.0),
                        );
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Close tab")
                    .on_hover_text("Close this cluster tab")
                    .clicked()
                {
                    cmds.push(TabCmd::Close);
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .button("Export kubeconfig")
                .on_hover_text("kind get kubeconfig + set as current context")
                .clicked()
            {
                cmds.push(TabCmd::Command(CoreCommand::EnsureContext {
                    name: tab.name.clone(),
                }));
            }
            ui.horizontal(|ui| {
                crate::icons::logo_image(ui, kindboard_core::ToolId::K9s, 16.0);
                if ui
                    .button("Open in k9s")
                    .on_hover_text("Open k9s for this cluster in a new terminal")
                    .clicked()
                {
                    cmds.push(TabCmd::Command(CoreCommand::OpenK9s {
                        name: tab.name.clone(),
                    }));
                }
            });
            if ui
                .button("Export logs")
                .on_hover_text("kind export logs to a directory")
                .clicked()
            {
                start_export_picker(tab);
            }
            if tab.has_record() {
                ui.separator();
                ui.label("workers:");
                let workers = ui.add(
                    egui::DragValue::new(&mut tab.workers).range(0..=kindboard_core::MAX_WORKERS),
                );
                if workers
                    .on_hover_text("Scaling recreates the cluster (kind has no node add/remove)")
                    .changed()
                {
                    // No-op: the stepper only stages the value.
                }
                if ui
                    .add_enabled(
                        tab.record
                            .as_ref()
                            .is_some_and(|record| record.spec.worker_count != tab.workers),
                        egui::Button::new("Apply scale"),
                    )
                    .on_hover_text("Apply: opens the recreate confirmation (workloads are lost)")
                    .clicked()
                {
                    tab.confirm = Some(TabConfirm::Recreate {
                        typed: String::new(),
                    });
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new(
                        RichText::new("Destroy").color(theme::RED),
                    ))
                    .on_hover_text("Destroy this cluster (confirmation required)")
                    .clicked()
                {
                    tab.confirm = Some(TabConfirm::Destroy {
                        typed: String::new(),
                    });
                }
            });
        });
        ui.add_space(2.0);
    });

    // ---- node list (left) ------------------------------------------------
    egui::Panel::left(egui::Id::new(("cluster-nodes", tab.name.clone())))
        .resizable(true)
        .default_size(190.0)
        .show(ui, |ui| {
            ui.strong("Nodes");
            ui.separator();
            match &tab.topo {
                Some(graph) => {
                    if graph.nodes.is_empty() {
                        ui.label(
                            RichText::new("no node data yet")
                                .color(theme::TEXT_DIM)
                                .size(11.0),
                        );
                    }
                    for node in &graph.nodes {
                        let node_id = format!("node/{}", node.name);
                        let is_selected = tab.selected.as_deref() == Some(node_id.as_str());
                        ui.horizontal(|ui| {
                            status_dot(
                                ui,
                                if node.ready {
                                    theme::GREEN
                                } else {
                                    theme::AMBER
                                },
                            );
                            if ui
                                .selectable_label(is_selected, &node.name)
                                .on_hover_text("Inspect this node")
                                .clicked()
                            {
                                tab.selected = if is_selected { None } else { Some(node_id) };
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| match node.role {
                                    NodeRole::ControlPlane => pill(ui, "CP", theme::ACCENT),
                                    NodeRole::Worker => pill(ui, "worker", theme::TEXT_DIM),
                                },
                            );
                        });
                    }
                }
                None => {
                    ui.label(
                        RichText::new("topology not loaded")
                            .color(theme::TEXT_DIM)
                            .size(11.0),
                    );
                }
            }
        });

    // ---- detail panel (right) --------------------------------------------
    egui::Panel::right(egui::Id::new(("cluster-detail", tab.name.clone())))
        .resizable(true)
        .default_size(290.0)
        .show(ui, |ui| {
            ui.strong("Details");
            ui.separator();
            let mut delete_request: Option<String> = None;
            match &tab.topo {
                Some(graph) => {
                    ScrollArea::vertical().show(ui, |ui| {
                        if let Some(diagram::DiagramAction::DeleteNode { node }) =
                            diagram::detail_panel(ui, graph, &tab.selected, tab.has_record())
                        {
                            delete_request = Some(node);
                        }
                    });
                }
                None => {
                    ui.label(
                        RichText::new("topology not loaded")
                            .color(theme::TEXT_DIM)
                            .size(11.0),
                    );
                }
            }
            if let Some(node) = delete_request {
                tab.confirm = Some(TabConfirm::DeleteNode {
                    node,
                    typed: String::new(),
                });
            }
        });

    // ---- logs (bottom) ----------------------------------------------------
    egui::Panel::bottom(egui::Id::new(("cluster-logs", tab.name.clone())))
        .resizable(true)
        .default_size(190.0)
        .show(ui, |ui| {
            ui.add_space(4.0);
            logs_section(ui, tab, &mut cmds);
        });

    // ---- topology diagram (center) ---------------------------------------
    egui::CentralPanel::default().show(ui, |ui| {
        // Toolbar.
        ui.horizontal(|ui| {
            if ui
                .button("Refresh now")
                .on_hover_text("Fetch a topology snapshot (Ctrl+R)")
                .clicked()
            {
                tab.manual_gen = tab.manual_gen.wrapping_add(1);
                tab.topo_loading = true;
                cmds.push(TabCmd::Command(CoreCommand::FetchTopology {
                    name: tab.name.clone(),
                    op_gen: tab.manual_gen,
                }));
            }
            let mut auto_enabled = tab.auto.is_some();
            if ui
                .checkbox(&mut auto_enabled, "Auto-refresh")
                .on_hover_text("Poll the cluster periodically")
                .changed()
            {
                let op_gen = tab
                    .auto
                    .as_ref()
                    .map(|auto| auto.op_gen.wrapping_add(1))
                    .unwrap_or(1);
                if auto_enabled {
                    let interval = tab.auto.as_ref().map_or(5, |auto| auto.interval_secs);
                    tab.auto = Some(AutoRefresh {
                        op_gen,
                        interval_secs: interval,
                    });
                } else {
                    tab.auto = None;
                }
                cmds.push(TabCmd::Command(CoreCommand::SetAutoRefresh {
                    name: tab.name.clone(),
                    interval_secs: tab.auto.as_ref().map_or(5, |auto| auto.interval_secs),
                    enabled: auto_enabled,
                    op_gen,
                }));
            }
            if let Some(auto) = &mut tab.auto {
                let mut interval = auto.interval_secs;
                let changed = egui::ComboBox::from_id_salt("auto-interval")
                    .selected_text(format!("every {interval}s"))
                    .width(96.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut interval, 5, "every 5s");
                        ui.selectable_value(&mut interval, 10, "every 10s");
                        ui.selectable_value(&mut interval, 30, "every 30s");
                    })
                    .response
                    .changed();
                if changed && interval != auto.interval_secs {
                    let op_gen = auto.op_gen.wrapping_add(1);
                    auto.interval_secs = interval;
                    auto.op_gen = op_gen;
                    cmds.push(TabCmd::Command(CoreCommand::SetAutoRefresh {
                        name: tab.name.clone(),
                        interval_secs: interval,
                        enabled: true,
                        op_gen,
                    }));
                }
            }
            ui.separator();
            if ui
                .button("Collapse all")
                .on_hover_text("Hide all namespace members")
                .clicked()
                && let Some(graph) = &tab.topo
            {
                for namespace in &graph.namespaces {
                    tab.collapsed.insert(namespace.name.clone());
                }
            }
            if ui
                .button("Expand all")
                .on_hover_text("Show all namespaces")
                .clicked()
            {
                tab.collapsed.clear();
            }
            if ui
                .button("Fit to view")
                .on_hover_text("Reset zoom and center the diagram")
                .clicked()
            {
                tab.diagram.fit_next = true;
            }
            if tab.topo_loading {
                ui.spinner();
            }
        });

        // Error banner + retry (never a blank pane).
        if let Some(error) = &tab.topo_error {
            egui::Frame::new()
                .fill(theme::dim(theme::RED))
                .stroke(egui::Stroke::new(1.0, theme::RED))
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("!").color(theme::RED).strong());
                        ui.label(
                            RichText::new(format!("Topology fetch failed: {error}"))
                                .color(theme::RED),
                        );
                        if ui.button("Retry").clicked() {
                            tab.topo_loading = true;
                            tab.manual_gen = tab.manual_gen.wrapping_add(1);
                            cmds.push(TabCmd::Command(CoreCommand::FetchTopology {
                                name: tab.name.clone(),
                                op_gen: tab.manual_gen,
                            }));
                        }
                    });
                });
        }

        // Canvas.
        match &tab.topo {
            Some(graph) => {
                let clicked = diagram::show(
                    ui,
                    graph,
                    &mut tab.collapsed,
                    &mut tab.diagram,
                    &tab.selected,
                );
                if let Some(id) = clicked {
                    // Clicking the same node again deselects it.
                    tab.selected = if tab.selected.as_deref() == Some(id.as_str()) {
                        None
                    } else {
                        Some(id)
                    };
                }
            }
            None if tab.topo_error.is_none() => {
                let available = ui.available_size();
                let (rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());
                ui.painter_at(rect).text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if tab.topo_loading {
                        "Loading topology..."
                    } else {
                        "No topology data yet — use Refresh now"
                    },
                    egui::FontId::proportional(13.0),
                    theme::TEXT_DIM,
                );
            }
            None => {
                // Error banner already shown above; keep the canvas empty.
                let available = ui.available_size();
                let (rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());
                ui.painter_at(rect).text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Topology unavailable",
                    egui::FontId::proportional(13.0),
                    theme::TEXT_DIM,
                );
            }
        }
    });

    // ---- export-logs picker polling --------------------------------------
    poll_picker(tab, &mut cmds);

    // ---- confirmations ----------------------------------------------------
    show_confirms(ui, tab, &mut cmds);

    cmds
}

/// Start the (blocking, native) directory picker on a helper thread; the
/// tab polls it every frame.
fn start_export_picker(tab: &mut ClusterTab) {
    let (tx, rx) = std::sync::mpsc::channel::<Option<PathBuf>>();
    std::thread::spawn(move || {
        let picked = rfd::FileDialog::new()
            .set_title("Export kind logs to directory")
            .pick_folder();
        let _ = tx.send(picked);
    });
    tab.picker = Some(rx);
}

/// Poll the directory picker; issue the export command when a directory
/// was chosen.
fn poll_picker(tab: &mut ClusterTab, cmds: &mut Vec<TabCmd>) {
    let Some(rx) = &tab.picker else {
        return;
    };
    match rx.try_recv() {
        Ok(Some(dir)) => {
            tab.picker = None;
            cmds.push(TabCmd::Command(CoreCommand::ExportLogs {
                name: tab.name.clone(),
                dest: dir,
            }));
        }
        Ok(None) => {
            tab.picker = None;
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {}
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            tab.picker = None;
        }
    }
}

/// Logs sub-view: source picker, follow/tail, start/stop, output ring.
fn logs_section(ui: &mut egui::Ui, tab: &mut ClusterTab, cmds: &mut Vec<TabCmd>) {
    ui.horizontal(|ui| {
        ui.strong("Logs");
        // Source picker. The picked source is staged into `new_source` and
        // applied after the match so the borrows stay disjoint.
        let mut source_changed = false;
        let mut new_source: Option<LogSourceKind> = None;
        let is_docker = matches!(tab.logs.source, LogSourceKind::Docker { .. });
        let mut picked_docker = is_docker;
        ui.radio_value(&mut picked_docker, true, "node container");
        ui.radio_value(&mut picked_docker, false, "pod");
        if picked_docker != is_docker {
            source_changed = true;
            if picked_docker {
                let container = tab
                    .topo
                    .as_ref()
                    .and_then(|graph| graph.nodes.first())
                    .map_or_else(
                        || format!("{}-control-plane", tab.name),
                        |node| node.name.clone(),
                    );
                new_source = Some(LogSourceKind::Docker { container });
            } else if let Some(pod) = tab.topo.as_ref().and_then(|graph| graph.pods.first()) {
                new_source = Some(LogSourceKind::Pod {
                    ns: pod.ns.clone(),
                    pod: pod.name.clone(),
                });
            }
        }

        let current_source = tab.logs.source.clone();
        match (&current_source, &tab.topo) {
            (LogSourceKind::Docker { container }, Some(graph)) => {
                let mut current = container.clone();
                egui::ComboBox::from_id_salt("log-container")
                    .selected_text(&current)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for node in &graph.nodes {
                            ui.selectable_value(&mut current, node.name.clone(), &node.name);
                        }
                    });
                if current != *container {
                    source_changed = true;
                    new_source = Some(LogSourceKind::Docker { container: current });
                }
            }
            (LogSourceKind::Pod { ns, pod }, Some(graph)) => {
                let mut current = format!("{ns}/{pod}");
                egui::ComboBox::from_id_salt("log-pod")
                    .selected_text(&current)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for graph_pod in &graph.pods {
                            let label = format!("{}/{}", graph_pod.ns, graph_pod.name);
                            ui.selectable_value(&mut current, label.clone(), label);
                        }
                    });
                if let Some((new_ns, new_pod)) = current.split_once('/')
                    && (new_ns != *ns || new_pod != *pod)
                {
                    source_changed = true;
                    new_source = Some(LogSourceKind::Pod {
                        ns: new_ns.to_string(),
                        pod: new_pod.to_string(),
                    });
                }
            }
            _ => {
                ui.label(
                    RichText::new("(topology needed for the picker)")
                        .color(theme::TEXT_DIM)
                        .size(11.0),
                );
            }
        }
        if let Some(source) = new_source {
            tab.logs.source = source;
        }

        if source_changed && tab.logs.running {
            // Restart the stream on source change (cancel + restart
            // semantics from core).
            tab.logs.op_gen = tab.logs.op_gen.wrapping_add(1);
            let source = tab.logs.to_core_source(&tab.name);
            cmds.push(TabCmd::Command(CoreCommand::StartLogs {
                name: tab.name.clone(),
                source,
                follow: tab.logs.follow,
                tail: Some(tab.logs.tail),
                op_gen: tab.logs.op_gen,
            }));
        }

        ui.separator();
        let follow = ui
            .checkbox(&mut tab.logs.follow, "Follow")
            .on_hover_text("Stream new lines instead of reading once");
        if follow.changed() && tab.logs.running {
            tab.logs.op_gen = tab.logs.op_gen.wrapping_add(1);
            let source = tab.logs.to_core_source(&tab.name);
            cmds.push(TabCmd::Command(CoreCommand::StartLogs {
                name: tab.name.clone(),
                source,
                follow: tab.logs.follow,
                tail: Some(tab.logs.tail),
                op_gen: tab.logs.op_gen,
            }));
        }
        if !tab.logs.follow {
            ui.label("tail");
            if ui
                .add(egui::DragValue::new(&mut tab.logs.tail).range(1..=5000))
                .on_hover_text("How many lines to read")
                .changed()
            {
                tab.logs.ended = None;
            }
        }

        if tab.logs.running {
            if ui
                .button("Stop")
                .on_hover_text("Stop the log stream")
                .clicked()
            {
                tab.logs.running = false;
                cmds.push(TabCmd::Command(CoreCommand::StopLogs {
                    name: tab.name.clone(),
                }));
            }
        } else {
            let enabled = matches!(
                tab.logs.source,
                LogSourceKind::Docker { .. } | LogSourceKind::Pod { .. }
            ) && tab.topo.is_some();
            if ui
                .add_enabled(enabled, egui::Button::new("Start"))
                .on_hover_text("Read logs from the selected source")
                .clicked()
            {
                tab.logs.op_gen = tab.logs.op_gen.wrapping_add(1);
                tab.logs.ring.clear();
                tab.logs.ended = None;
                tab.logs.running = true;
                let source = tab.logs.to_core_source(&tab.name);
                cmds.push(TabCmd::Command(CoreCommand::StartLogs {
                    name: tab.name.clone(),
                    source,
                    follow: tab.logs.follow,
                    tail: Some(tab.logs.tail),
                    op_gen: tab.logs.op_gen,
                }));
            }
        }
        if let Some(ended) = &tab.logs.ended {
            match ended {
                Ok(()) => {
                    ui.label(
                        RichText::new("stream ended")
                            .color(theme::TEXT_DIM)
                            .size(11.0),
                    );
                }
                Err(err) => {
                    ui.label(
                        RichText::new(format!("log error: {err}"))
                            .color(theme::RED)
                            .size(11.0),
                    );
                }
            }
        }
    });

    // Truncation notice.
    if tab.logs.ring.len() >= tab.logs.ring.capacity() {
        ui.label(
            RichText::new(format!(
                "showing the last {} lines (older lines dropped)",
                tab.logs.ring.capacity()
            ))
            .color(theme::AMBER)
            .size(11.0),
        );
    }

    // Output ring.
    let available_height = ui.available_height().max(60.0);
    let lines = tab.logs.ring.snapshot();
    ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .max_height(available_height)
        .show(ui, |ui| {
            if lines.is_empty() {
                ui.label(
                    RichText::new("no log output yet")
                        .color(theme::TEXT_DIM)
                        .monospace()
                        .size(11.0),
                );
            }
            for line in lines {
                ui.label(
                    RichText::new(line)
                        .color(theme::TEXT_DIM)
                        .monospace()
                        .size(11.0),
                );
            }
            if tab.logs.running {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("following...").monospace().size(11.0));
                });
            }
        });
}

/// Render the confirmation dialogs.
fn show_confirms(ui: &mut egui::Ui, tab: &mut ClusterTab, cmds: &mut Vec<TabCmd>) {
    let Some(confirm) = tab.confirm.take() else {
        return;
    };
    let (title, body, action): (&str, String, ConfirmAction) = match confirm {
        TabConfirm::Destroy { typed } => (
            "Destroy cluster",
            "Destroying a cluster deletes its node containers, its kubeconfig context and \
             its stored record. All workloads and data on the cluster are lost forever."
                .to_string(),
            ConfirmAction::Destroy { typed },
        ),
        TabConfirm::Recreate { typed } => {
            let summary = tab
                .spec_with_workers()
                .map(|spec| format!("workers {}", spec.worker_count))
                .unwrap_or_else(|| "workers ?".to_string());
            (
                "Apply scale (recreate)",
                format!(
                    "kind cannot add or remove nodes on a running cluster. Scaling recreates \
                     the cluster from its stored spec with the new value ({summary}): the \
                     old cluster is deleted and all workloads on it are lost."
                ),
                ConfirmAction::Recreate { typed },
            )
        }
        TabConfirm::DeleteNode { node, typed } => {
            let target = tab.workers.saturating_sub(1);
            (
                "Delete node",
                format!(
                    "kind cannot remove a node from a running cluster. Deleting worker \
                     '{node}' recreates the cluster with {target} worker(s) from its stored \
                     settings: the old cluster is deleted and all workloads on it are lost."
                ),
                ConfirmAction::DeleteNode { node, typed },
            )
        }
    };

    let mut confirmed = false;
    let mut cancelled = false;
    let mut typed = match &action {
        ConfirmAction::Destroy { typed }
        | ConfirmAction::Recreate { typed }
        | ConfirmAction::DeleteNode { typed, .. } => typed.clone(),
    };
    let modal = Modal::new(egui::Id::new("tab-confirm")).show(ui.ctx(), |ui| {
        ui.set_width(430.0);
        ui.heading(title);
        ui.add_space(4.0);
        ui.label(RichText::new(body).color(theme::RED));
        ui.add_space(6.0);
        ui.label("Type the cluster name to confirm:");
        ui.label(RichText::new(&tab.name).strong().monospace());
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::singleline(&mut typed)
                .hint_text(&tab.name)
                .desired_width(390.0),
        );
        ui.add_space(8.0);
        let matches = typed.trim() == tab.name;
        ui.horizontal(|ui| {
            let label = match action {
                ConfirmAction::Destroy { .. } => "Destroy",
                ConfirmAction::DeleteNode { .. } => "Delete & recreate",
                ConfirmAction::Recreate { .. } => "Recreate",
            };
            let fill = match action {
                ConfirmAction::Destroy { .. } | ConfirmAction::DeleteNode { .. } => theme::RED,
                ConfirmAction::Recreate { .. } => theme::AMBER,
            };
            let button = ui.add_enabled(
                matches,
                egui::Button::new(RichText::new(label).strong())
                    .fill(fill)
                    .min_size(egui::vec2(130.0, 30.0)),
            );
            if button.on_hover_text("Confirm").clicked() {
                confirmed = true;
            }
            if ui.button("Cancel").clicked() {
                cancelled = true;
            }
        });
        if !matches && !typed.is_empty() {
            ui.label(
                RichText::new("the typed name does not match")
                    .color(theme::RED)
                    .size(11.0),
            );
        }
    });
    if modal.should_close() || modal.backdrop_response.clicked() {
        cancelled = true;
    }

    if confirmed {
        match action {
            ConfirmAction::Destroy { .. } => {
                cmds.push(TabCmd::Destroy {
                    name: tab.name.clone(),
                });
            }
            ConfirmAction::Recreate { .. } => {
                if let Some(spec) = tab.spec_with_workers() {
                    cmds.push(TabCmd::Recreate {
                        spec: Box::new(spec),
                    });
                }
            }
            ConfirmAction::DeleteNode { .. } => {
                // One fewer worker; the guided recreate does the rest.
                tab.workers = tab.workers.saturating_sub(1);
                if let Some(spec) = tab.spec_with_workers() {
                    cmds.push(TabCmd::Recreate {
                        spec: Box::new(spec),
                    });
                }
            }
        }
    } else if !cancelled {
        tab.confirm = Some(match action {
            ConfirmAction::Destroy { .. } => TabConfirm::Destroy { typed },
            ConfirmAction::Recreate { .. } => TabConfirm::Recreate { typed },
            ConfirmAction::DeleteNode { node, .. } => TabConfirm::DeleteNode { node, typed },
        });
    }
}

/// What a confirmation dialog commits to.
enum ConfirmAction {
    Destroy { typed: String },
    Recreate { typed: String },
    DeleteNode { node: String, typed: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use kindboard_core::ClusterRecord;

    fn record(workers: u32) -> ClusterRecord {
        let spec = kindboard_core::ClusterSpec {
            name: "demo".to_string(),
            worker_count: workers,
            ..kindboard_core::ClusterSpec::default()
        };
        ClusterRecord::managed(spec)
    }

    #[test]
    fn tab_takes_worker_count_from_record() {
        let tab = ClusterTab::new("demo".to_string(), Some(record(3)));
        assert_eq!(tab.workers, 3);
    }

    #[test]
    fn adopted_tabs_have_no_record() {
        let tab = ClusterTab::new("demo".to_string(), None);
        assert!(!tab.has_record());
        assert!(tab.spec_with_workers().is_none());
    }

    #[test]
    fn spec_with_workers_replaces_worker_count() {
        let mut tab = ClusterTab::new("demo".to_string(), Some(record(1)));
        tab.workers = 5;
        let spec = tab.spec_with_workers();
        assert_eq!(spec.map(|s| s.worker_count), Some(5));
    }

    #[test]
    fn topology_generation_routing() {
        let mut tab = ClusterTab::new("demo".to_string(), None);
        tab.manual_gen = 7;
        assert!(tab.accept_topology(7));
        assert!(!tab.accept_topology(6));
        tab.auto = Some(AutoRefresh {
            op_gen: 9,
            interval_secs: 5,
        });
        assert!(tab.accept_topology(9));
        assert!(!tab.accept_topology(10));
    }

    #[test]
    fn log_generation_routing() {
        let mut tab = ClusterTab::new("demo".to_string(), None);
        tab.logs.op_gen = 4;
        assert!(tab.accept_log(4));
        assert!(!tab.accept_log(3));
    }

    #[test]
    fn docker_log_source_builds_container() {
        let tab = ClusterTab::new("demo".to_string(), None);
        match tab.logs.source {
            LogSourceKind::Docker { container } => {
                assert_eq!(container, "demo-control-plane");
            }
            _ => panic!("fresh tabs default to the docker node source"),
        }
    }
}
