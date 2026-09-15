//! Overview tab: the cluster grid (reconcile report + live status) on the
//! left, the dependency panel on the right, empty states, and the destroy
//! confirmation modal.

use std::collections::HashMap;

use eframe::egui::{self, Modal, RichText};
use kindboard_core::{ClusterRecord, ClusterState, ReconcileReport};

use crate::bus::{ClusterLiveStatus, CoreCommand};
use crate::theme;
use crate::util::pill;
use crate::views::deps::DepsState;

/// Overview tab state.
#[derive(Default)]
pub struct OverviewState {
    /// Reconcile in flight.
    pub loading: bool,
    /// Live cluster names (`kind get clusters`).
    pub live: Vec<String>,
    /// Per-cluster live status (running/creating/absent + node count).
    pub statuses: HashMap<String, ClusterLiveStatus>,
    /// Last reconcile report.
    pub report: Option<ReconcileReport>,
    /// Reconcile failure (kind missing etc.).
    pub error: Option<String>,
    /// Dependency panel state.
    pub deps: DepsState,
    /// Open destroy confirmation.
    pub destroy: Option<DestroyConfirm>,
}

/// Typed-name confirmation state (destroy or scale).
#[derive(Default)]
pub struct DestroyConfirm {
    /// Cluster being destroyed/scaled.
    pub name: String,
    /// What the user typed so far.
    pub typed: String,
    /// Whether this destroys only the stored record (missing cluster).
    pub record_only: bool,
    /// Scale target: when `Some`, this confirms a guided recreate to that
    /// worker count instead of a destroy.
    pub scale_to: Option<u32>,
}

/// Actions the overview wants the app to perform (gens are assigned by the
/// app, which owns the op-panel bookkeeping).
pub enum OverviewCmd {
    /// Enqueue a plain command.
    Command(CoreCommand),
    /// Destroy a cluster (app assigns the op generation + opens the panel).
    Destroy {
        /// Cluster name.
        name: String,
        /// Missing cluster: only the stored record remains.
        record_only: bool,
    },
    /// Recreate a missing cluster from its stored spec.
    Recreate {
        /// Cluster name.
        name: String,
    },
    /// Guided recreate with a new worker count (scale up/down).
    Scale {
        /// Cluster name.
        name: String,
        /// New worker count.
        workers: u32,
    },
    /// Open the cluster tab.
    OpenCluster(String),
}

/// Render the overview. Returns commands to execute.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut OverviewState,
    open_wizard: &mut bool,
) -> Vec<OverviewCmd> {
    let mut cmds: Vec<OverviewCmd> = Vec::new();

    // Dependency readiness summary for the dashboard chip + notice card
    // (R7: the checked deps reflect on the dashboard).
    let deps_ready = deps_summary(&state.deps);

    ui.horizontal_wrapped(|ui| {
        ui.strong("Clusters");
        let create = ui
            .add(
                egui::Button::new(
                    RichText::new("Create cluster")
                        .strong()
                        .color(theme::pal().on_accent),
                )
                .fill(theme::pal().accent)
                .min_size(egui::vec2(130.0, 30.0)),
            )
            .on_hover_text("Open the create wizard");
        if create.clicked() {
            *open_wizard = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let refresh = ui
                .add_enabled(!state.loading, egui::Button::new("Refresh"))
                .on_hover_text("Re-run kind get clusters + reconcile");
            if refresh.clicked() {
                cmds.push(OverviewCmd::Command(CoreCommand::Reconcile));
            }
            // Tools chip: reflects the last dependency check.
            let (chip_text, chip_color) = deps_chip(&deps_ready);
            ui.label(
                RichText::new(chip_text)
                    .color(chip_color)
                    .size(12.0)
                    .strong(),
            )
            .on_hover_text(deps_chip_hover(&deps_ready));
        });
    });

    // Critical dependencies missing/broken: the dashboard surfaces it
    // instead of leaving the user to discover it in the side panel.
    if let Some(critical) = deps_ready.critical_missing.first() {
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(theme::pal().dim(theme::pal().amber))
            .stroke(egui::Stroke::new(1.0, theme::pal().amber))
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} is required to create clusters",
                            critical.display_name
                        ))
                        .color(theme::pal().amber),
                    );
                    if ui
                        .button(format!("Install {}", critical.display_name))
                        .clicked()
                    {
                        cmds.push(OverviewCmd::Command(CoreCommand::InstallTool {
                            id: critical.id,
                        }));
                    }
                    if ui.button("Check again").clicked() {
                        let run = state.deps.begin_detect();
                        cmds.push(OverviewCmd::Command(CoreCommand::DetectTools { run }));
                        cmds.push(OverviewCmd::Command(CoreCommand::CheckDockerDaemon));
                    }
                });
            });
    }

    if let Some(error) = &state.error {
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(theme::pal().dim(theme::pal().red))
            .stroke(egui::Stroke::new(1.0, theme::pal().red))
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("!").color(theme::pal().red).strong());
                    ui.label(
                        RichText::new(format!("Cluster listing failed: {error}"))
                            .color(theme::pal().red),
                    );
                    if ui.button("Retry").clicked() {
                        cmds.push(OverviewCmd::Command(CoreCommand::Reconcile));
                    }
                });
            });
    }

    ui.add_space(6.0);

    let mut cluster_rows: Vec<OverviewRow> = Vec::new();
    if let Some(report) = &state.report {
        cluster_rows = report
            .clusters
            .iter()
            .map(|entry| OverviewRow::from_entry(entry, &state.statuses))
            .collect();
    } else if state.loading {
        // Loading indicator row.
    }

    if cluster_rows.is_empty() && !state.loading {
        egui::Frame::new()
            .fill(theme::pal().bg_raised)
            .stroke(egui::Stroke::new(1.0, theme::pal().stroke))
            .corner_radius(egui::CornerRadius::same(6))
            .inner_margin(egui::Margin::same(24))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new("No clusters yet").size(16.0).strong());
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Create your first kind cluster to get started.")
                            .color(theme::pal().text_dim),
                    );
                    ui.add_space(10.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Create your first cluster")
                                    .strong()
                                    .color(theme::pal().on_accent),
                            )
                            .fill(theme::pal().accent)
                            .min_size(egui::vec2(200.0, 32.0)),
                        )
                        .clicked()
                    {
                        *open_wizard = true;
                    }
                    ui.add_space(8.0);
                });
            });
    } else if state.loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("reconciling clusters...").color(theme::pal().text_dim));
        });
    }

    // Cluster cards.
    let mut open_name: Option<String> = None;
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for row in &cluster_rows {
                render_card(ui, row, &mut cmds, &mut open_name, state);
            }
        });
    });
    if let Some(name) = open_name {
        cmds.push(OverviewCmd::OpenCluster(name));
    }

    ui.add_space(8.0);

    // Destroy/scale confirmation modal (state is taken out for the frame to
    // keep the borrow checker happy without unsafe shortcuts).
    if let Some(mut confirm) = state.destroy.take() {
        let mut confirmed = false;
        let mut cancelled = false;
        let modal = Modal::new(egui::Id::new("destroy-confirm")).show(ui.ctx(), |ui| {
            ui.set_width(ui.available_width().min(400.0));
            match confirm.scale_to {
                Some(workers) => {
                    ui.heading("Apply scale (recreate)");
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "kind cannot add or remove nodes on a running cluster. Scaling to \
                             {workers} worker(s) recreates the cluster from its stored spec: \
                             the old cluster is deleted and all workloads on it are lost."
                        ))
                        .color(theme::pal().red),
                    );
                }
                None => {
                    ui.heading("Destroy cluster");
                    ui.add_space(4.0);
                    if confirm.record_only {
                        ui.label(
                            RichText::new(
                                "This cluster is already gone (only its stored record remains). \
                                 Removing the record and its kubeconfig context cannot be undone.",
                            )
                            .color(theme::pal().red),
                        );
                    } else {
                        ui.label(
                            RichText::new(
                                "Destroying a cluster deletes its node containers, its kubeconfig \
                                 context and its stored record. All workloads and data on the \
                                 cluster are lost forever.",
                            )
                            .color(theme::pal().red),
                        );
                    }
                }
            }
            ui.add_space(6.0);
            ui.label("Type the cluster name to confirm:");
            ui.label(RichText::new(&confirm.name).strong().monospace());
            ui.add_space(4.0);
            ui.add(
                egui::TextEdit::singleline(&mut confirm.typed)
                    .hint_text(&confirm.name)
                    .desired_width(f32::INFINITY),
            );
            ui.add_space(8.0);
            let matches = confirm.typed.trim() == confirm.name;
            let (label, fill) = match confirm.scale_to {
                Some(_) => ("Scale", theme::pal().amber),
                None => ("Destroy", theme::pal().red),
            };
            ui.horizontal(|ui| {
                let commit = ui.add_enabled(
                    matches,
                    egui::Button::new(
                        RichText::new(label)
                            .strong()
                            .color(crate::util::on_fill(fill)),
                    )
                    .fill(fill)
                    .min_size(egui::vec2(110.0, 30.0)),
                );
                if commit.on_hover_text("Confirm").clicked() {
                    confirmed = true;
                }
                if ui.button("Cancel").clicked() {
                    cancelled = true;
                }
            });
            if !matches && !confirm.typed.is_empty() {
                ui.label(
                    RichText::new("the typed name does not match")
                        .color(theme::pal().red)
                        .size(11.0),
                );
            }
        });
        if modal.should_close() || modal.backdrop_response.clicked() {
            cancelled = true;
        }
        if confirmed {
            match confirm.scale_to {
                Some(workers) => cmds.push(OverviewCmd::Scale {
                    name: confirm.name,
                    workers,
                }),
                None => cmds.push(OverviewCmd::Destroy {
                    name: confirm.name,
                    record_only: confirm.record_only,
                }),
            }
        } else if !cancelled {
            state.destroy = Some(confirm);
        }
    }

    cmds
}

/// A flattened row for one cluster (reconcile + live status joined).
struct OverviewRow {
    name: String,
    record: Option<ClusterRecord>,
    source_label: &'static str,
    status: Option<ClusterLiveStatus>,
}

impl OverviewRow {
    fn from_entry(
        entry: &kindboard_core::ClusterEntry,
        statuses: &HashMap<String, ClusterLiveStatus>,
    ) -> Self {
        match &entry.state {
            ClusterState::Managed(record) => OverviewRow {
                name: entry.name.clone(),
                record: Some(record.clone()),
                source_label: "Managed",
                status: statuses.get(&entry.name).copied(),
            },
            ClusterState::Adopted => OverviewRow {
                name: entry.name.clone(),
                record: None,
                source_label: "Adopted",
                status: statuses.get(&entry.name).copied(),
            },
            ClusterState::Missing(record) => OverviewRow {
                name: entry.name.clone(),
                record: Some(record.clone()),
                source_label: "Missing",
                status: None,
            },
        }
    }
}

/// One critical tool that is missing or broken on the dashboard.
struct CriticalTool {
    /// Tool id (for the install command).
    id: kindboard_core::ToolId,
    /// Registry display name.
    display_name: String,
}

/// Aggregated dependency readiness derived from the last check (R7).
struct DepsReady {
    /// Total tools in the registry.
    total: usize,
    /// Tools reported installed.
    installed: usize,
    /// Tools with no result yet (still unchecked).
    unchecked: usize,
    /// Detection in flight.
    detecting: bool,
    /// Missing/broken/failed tools (id + display name), registry order.
    problems: Vec<(kindboard_core::ToolId, String)>,
    /// docker/kind when unavailable — creation blockers.
    critical_missing: Vec<CriticalTool>,
}

fn deps_summary(deps: &DepsState) -> DepsReady {
    let mut summary = DepsReady {
        total: kindboard_core::registry().len(),
        installed: 0,
        unchecked: 0,
        detecting: deps.detecting,
        problems: Vec::new(),
        critical_missing: Vec::new(),
    };
    for tool in kindboard_core::registry() {
        match deps.results.get(&tool.id) {
            Some(Ok(kindboard_core::ToolStatus::Installed { .. })) => {
                summary.installed += 1;
            }
            Some(Ok(_)) | Some(Err(_)) => {
                summary.problems.push((tool.id, tool.display.to_string()));
            }
            None => {
                summary.unchecked += 1;
            }
        }
    }
    for (id, display_name) in &summary.problems {
        if matches!(
            id,
            kindboard_core::ToolId::Docker | kindboard_core::ToolId::Kind
        ) {
            summary.critical_missing.push(CriticalTool {
                id: *id,
                display_name: display_name.clone(),
            });
        }
    }
    summary
}

/// Text + color for the dashboard tools chip.
fn deps_chip(summary: &DepsReady) -> (String, eframe::egui::Color32) {
    if summary.detecting && summary.installed == 0 && summary.problems.is_empty() {
        return ("Tools: checking…".to_string(), theme::pal().text_dim);
    }
    if summary.problems.is_empty() && summary.unchecked == 0 {
        (
            format!("Tools: {}/{} ready", summary.installed, summary.total),
            theme::pal().green,
        )
    } else if summary.problems.is_empty() {
        (
            format!("Tools: {}/{} checked", summary.installed, summary.total),
            theme::pal().text_dim,
        )
    } else {
        (
            format!("Tools: {}/{} ready", summary.installed, summary.total),
            theme::pal().amber,
        )
    }
}

/// Hover text listing exactly which tools are missing/broken.
fn deps_chip_hover(summary: &DepsReady) -> String {
    if summary.detecting {
        return "Dependency check in progress".to_string();
    }
    if summary.problems.is_empty() {
        return format!("{} tools checked", summary.total);
    }
    let names: Vec<&str> = summary
        .problems
        .iter()
        .map(|(_, display)| display.as_str())
        .collect();
    format!("missing/broken: {}", names.join(", "))
}

/// Fixed content height of a cluster card: every card renders the same
/// four rows inside fixed-size regions, so all cards have the same size
/// regardless of cluster state (R12).
const CARD_CONTENT_HEIGHT: f32 = 150.0;
/// Fixed height of the buttons region (two wrapped rows at card width).
const CARD_BUTTONS_HEIGHT: f32 = 56.0;
/// Fixed height of the nodes row.
const CARD_NODES_ROW_HEIGHT: f32 = 26.0;
/// Cluster names are truncated to one header line.
const CARD_NAME_CHARS: usize = 24;

fn render_card(
    ui: &mut egui::Ui,
    row: &OverviewRow,
    cmds: &mut Vec<OverviewCmd>,
    open_name: &mut Option<String>,
    state: &mut OverviewState,
) {
    egui::Frame::new()
        .fill(theme::pal().bg_raised)
        .stroke(egui::Stroke::new(1.0, theme::pal().stroke))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            // The overview grid is a wrapped layout; `Frame::show` inherits
            // it, which would flow the card content horizontally. Force a
            // vertical stack so the fixed rows really stack (R12: every
            // card has the same size).
            ui.vertical(|ui| {
            // Cards shrink with the panel and wrap to one column at small
            // widths (R3) — never clip. The inner rows are fixed-size, so
            // every card ends up exactly the same size.
            let width = ui.available_width().min(330.0);
            ui.set_width(width);
            ui.set_min_height(CARD_CONTENT_HEIGHT);

            // Row 1: name + status pill (one line, truncated).
            ui.horizontal(|ui| {
                ui.strong(crate::util::truncate(&row.name, CARD_NAME_CHARS))
                    .on_hover_text(&row.name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    status_pill(ui, row.source_label, row.status);
                });
            });

            // Row 2: spec summary (one line).
            let version = row
                .record
                .as_ref()
                .map(|record| record.spec.k8s_version.as_str().to_string());
            let workers = row
                .record
                .as_ref()
                .map(|record| record.spec.worker_count);
            let cni = row
                .record
                .as_ref()
                .map(|record| format!("{:?}", record.spec.cni))
                .unwrap_or_else(|| "unknown".to_string());
            let spec_line = match (workers, &row.status) {
                (Some(workers), _) => {
                    format!("k8s {} | {cni} | {workers} workers", version_unwrap(version.as_deref()))
                }
                (None, Some(ClusterLiveStatus::Running { node_count })) => {
                    format!("k8s ? | {cni} | {node_count} nodes")
                }
                (None, _) => format!("k8s ? | {cni}"),
            };
            ui.label(
                RichText::new(spec_line)
                    .color(theme::pal().text_dim)
                    .size(11.0),
            );
            ui.add_space(6.0);

            // Row 3: actions — always exactly four buttons so every card
            // wraps identically: [Open|Recreate] [k9s] [Export kubeconfig]
            // [Destroy|Discard record].
            let mut destroy_request: Option<(String, bool)> = None;
            let mut open = false;
            let interactive = row.record.is_some() || row.status.is_some();
            ui.allocate_ui_with_layout(
                egui::vec2(width, CARD_BUTTONS_HEIGHT),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if row.source_label == "Missing" {
                            if ui
                                .button("Recreate")
                                .on_hover_text(
                                    "Recreate the cluster from its stored spec (workloads are gone)",
                                )
                                .clicked()
                            {
                                cmds.push(OverviewCmd::Recreate {
                                    name: row.name.clone(),
                                });
                            }
                        } else {
                            let open_button = ui.add_enabled(interactive, egui::Button::new("Open"));
                            if open_button
                                .on_hover_text("Open the cluster tab")
                                .clicked()
                            {
                                open = true;
                            }
                        }
                        ui.horizontal(|ui| {
                            crate::icons::logo_image(ui, kindboard_core::ToolId::K9s, 14.0);
                            if ui
                                .add_enabled(interactive, egui::Button::new("k9s"))
                                .on_hover_text("Open k9s for this cluster in a new terminal")
                                .clicked()
                            {
                                cmds.push(OverviewCmd::Command(CoreCommand::OpenK9s {
                                    name: row.name.clone(),
                                }));
                            }
                        });
                        if ui
                            .button("Export kubeconfig")
                            .on_hover_text("kind get kubeconfig + set as current context")
                            .clicked()
                        {
                            cmds.push(OverviewCmd::Command(CoreCommand::EnsureContext {
                                name: row.name.clone(),
                            }));
                        }
                        let destroy_label = if row.source_label == "Missing" {
                            "Discard record"
                        } else {
                            "Destroy"
                        };
                        if ui
                            .add(egui::Button::new(
                                RichText::new(destroy_label).color(theme::pal().red),
                            ))
                            .on_hover_text("Open the destroy confirmation")
                            .clicked()
                        {
                            destroy_request =
                                Some((row.name.clone(), row.source_label == "Missing"));
                        }
                    });
                },
            );

            // Row 4: nodes — interactive stepper for managed clusters,
            // static count for adopted/missing (same fixed height).
            ui.allocate_ui_with_layout(
                egui::vec2(width, CARD_NODES_ROW_HEIGHT),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    if let Some(record) = &row.record
                        && row.source_label != "Missing"
                    {
                        let mut workers = record.spec.worker_count;
                        let mut apply_request = false;
                        ui.label(
                            RichText::new("nodes:").size(11.0).color(theme::pal().text_dim),
                        );
                        let response = ui.add(
                            egui::DragValue::new(&mut workers)
                                .range(0..=kindboard_core::MAX_WORKERS),
                        );
                        let changed = workers != record.spec.worker_count;
                        response.on_hover_text(
                            "Worker nodes (the control-plane is always added). Scaling recreates the cluster.",
                        );
                        let apply = ui.add_enabled(
                            changed,
                            egui::Button::new(
                                RichText::new(if workers > record.spec.worker_count {
                                    "Scale up"
                                } else {
                                    "Scale down"
                                })
                                .size(11.0),
                            ),
                        );
                        if apply
                            .on_hover_text(
                                "Apply: opens the confirmation (recreates the cluster; workloads are lost)",
                            )
                            .clicked()
                        {
                            apply_request = true;
                        }
                        if apply_request {
                            state.destroy = Some(DestroyConfirm {
                                name: row.name.clone(),
                                typed: String::new(),
                                record_only: false,
                                scale_to: Some(workers),
                            });
                        }
                    } else {
                        let nodes_text = match &row.status {
                            Some(ClusterLiveStatus::Running { node_count }) => {
                                format!("nodes: {node_count}")
                            }
                            _ => "nodes: —".to_string(),
                        };
                        ui.label(
                            RichText::new(nodes_text)
                                .size(11.0)
                                .color(theme::pal().text_dim),
                        );
                    }
                },
            );

            if open {
                *open_name = Some(row.name.clone());
            }
            if let Some((name, record_only)) = destroy_request {
                state.destroy = Some(DestroyConfirm {
                    name,
                    typed: String::new(),
                    record_only,
                    scale_to: None,
                });
            }
            });
        });
}

fn version_unwrap(version: Option<&str>) -> String {
    version.map_or_else(|| "?".to_string(), std::string::ToString::to_string)
}

fn status_pill(ui: &mut egui::Ui, source: &str, live: Option<ClusterLiveStatus>) {
    match (source, live) {
        (_, Some(ClusterLiveStatus::Running { node_count })) => {
            pill(
                ui,
                format!("running ({node_count} nodes)"),
                theme::pal().green,
            );
        }
        (_, Some(ClusterLiveStatus::Creating)) => {
            pill(ui, "creating", theme::pal().amber);
        }
        (_, Some(ClusterLiveStatus::Absent)) => {
            pill(ui, "absent", theme::pal().grey);
        }
        ("Managed", None) => {
            pill(ui, "managed (down?)", theme::pal().amber);
        }
        ("Adopted", None) => {
            pill(ui, "adopted", theme::pal().grey);
        }
        ("Missing", None) => {
            pill(ui, "missing", theme::pal().red);
        }
        _ => {
            pill(ui, source, theme::pal().grey);
        }
    }
}
