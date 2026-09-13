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

    ui.horizontal(|ui| {
        ui.strong("Clusters");
        let create = ui
            .add(
                egui::Button::new(RichText::new("Create cluster").strong())
                    .fill(theme::ACCENT)
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
        });
    });

    if let Some(error) = &state.error {
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(theme::dim(theme::RED))
            .stroke(egui::Stroke::new(1.0, theme::RED))
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("!").color(theme::RED).strong());
                    ui.label(
                        RichText::new(format!("Cluster listing failed: {error}")).color(theme::RED),
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
            .fill(theme::BG_RAISED)
            .stroke(egui::Stroke::new(1.0, theme::STROKE))
            .corner_radius(egui::CornerRadius::same(6))
            .inner_margin(egui::Margin::same(24))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new("No clusters yet").size(16.0).strong());
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Create your first kind cluster to get started.")
                            .color(theme::TEXT_DIM),
                    );
                    ui.add_space(10.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Create your first cluster").strong())
                                .fill(theme::ACCENT)
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
            ui.label(RichText::new("reconciling clusters...").color(theme::TEXT_DIM));
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
            ui.set_width(400.0);
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
                        .color(theme::RED),
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
                            .color(theme::RED),
                        );
                    } else {
                        ui.label(
                            RichText::new(
                                "Destroying a cluster deletes its node containers, its kubeconfig \
                                 context and its stored record. All workloads and data on the \
                                 cluster are lost forever.",
                            )
                            .color(theme::RED),
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
                    .desired_width(360.0),
            );
            ui.add_space(8.0);
            let matches = confirm.typed.trim() == confirm.name;
            let (label, fill) = match confirm.scale_to {
                Some(_) => ("Scale", theme::AMBER),
                None => ("Destroy", theme::RED),
            };
            ui.horizontal(|ui| {
                let commit = ui.add_enabled(
                    matches,
                    egui::Button::new(RichText::new(label).strong())
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
                        .color(theme::RED)
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

fn render_card(
    ui: &mut egui::Ui,
    row: &OverviewRow,
    cmds: &mut Vec<OverviewCmd>,
    open_name: &mut Option<String>,
    state: &mut OverviewState,
) {
    egui::Frame::new()
        .fill(theme::BG_RAISED)
        .stroke(egui::Stroke::new(1.0, theme::STROKE))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(330.0);
            ui.horizontal(|ui| {
                ui.strong(&row.name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    status_pill(ui, row.source_label, row.status);
                });
            });
            ui.add_space(4.0);
            let version = row
                .record
                .as_ref()
                .map(|record| record.spec.k8s_version.as_str().to_string());
            let workers = row
                .record
                .as_ref()
                .map(|record| record.spec.worker_count)
                .unwrap_or(0);
            let cni = row
                .record
                .as_ref()
                .map(|record| format!("{:?}", record.spec.cni))
                .unwrap_or_else(|| "unknown".to_string());
            ui.label(
                RichText::new(format!(
                    "k8s {} | {cni} | {workers} workers",
                    version_unwrap(version.as_deref())
                ))
                .color(theme::TEXT_DIM)
                .size(11.0),
            );
            ui.add_space(6.0);

            let mut destroy_request: Option<(String, bool)> = None;
            let mut open = false;
            ui.horizontal_wrapped(|ui| {
                if (row.record.is_some() || row.status.is_some())
                    && ui
                        .button("Open")
                        .on_hover_text("Open the cluster tab")
                        .clicked()
                {
                    open = true;
                }
                ui.horizontal(|ui| {
                    crate::icons::logo_image(ui, kindboard_core::ToolId::K9s, 14.0);
                    if ui
                        .button("k9s")
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
                if row.source_label == "Missing"
                    && ui
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
                // Scale up/down (guided recreate) for managed clusters: the
                // stepper only stages a value; committing requires the
                // typed-name confirmation like every other destructive
                // action.
                if let Some(record) = &row.record
                    && row.source_label != "Missing"
                {
                    let mut workers = record.spec.worker_count;
                    let mut apply_request = false;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("nodes:").size(11.0).color(theme::TEXT_DIM));
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
                            .on_hover_text("Apply: opens the confirmation (recreates the cluster; workloads are lost)")
                            .clicked()
                        {
                            apply_request = true;
                        }
                    });
                    if apply_request {
                        state.destroy = Some(DestroyConfirm {
                            name: row.name.clone(),
                            typed: String::new(),
                            record_only: false,
                            scale_to: Some(workers),
                        });
                    }
                }
                let destroy_label = if row.source_label == "Missing" {
                    "Discard record"
                } else {
                    "Destroy"
                };
                if ui
                    .add(egui::Button::new(
                        RichText::new(destroy_label).color(theme::RED),
                    ))
                    .on_hover_text("Open the destroy confirmation")
                    .clicked()
                {
                    destroy_request = Some((row.name.clone(), row.source_label == "Missing"));
                }
            });
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
}

fn version_unwrap(version: Option<&str>) -> String {
    version.map_or_else(|| "?".to_string(), std::string::ToString::to_string)
}

fn status_pill(ui: &mut egui::Ui, source: &str, live: Option<ClusterLiveStatus>) {
    match (source, live) {
        (_, Some(ClusterLiveStatus::Running { node_count })) => {
            pill(ui, format!("running ({node_count} nodes)"), theme::GREEN);
        }
        (_, Some(ClusterLiveStatus::Creating)) => {
            pill(ui, "creating", theme::AMBER);
        }
        (_, Some(ClusterLiveStatus::Absent)) => {
            pill(ui, "absent", theme::GREY);
        }
        ("Managed", None) => {
            pill(ui, "managed (down?)", theme::AMBER);
        }
        ("Adopted", None) => {
            pill(ui, "adopted", theme::GREY);
        }
        ("Missing", None) => {
            pill(ui, "missing", theme::RED);
        }
        _ => {
            pill(ui, source, theme::GREY);
        }
    }
}
