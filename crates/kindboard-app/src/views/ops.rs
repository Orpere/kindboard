//! Per-operation output panel: the dedicated streamed view for
//! create/destroy/recreate runs. Shows a step timeline (derived from
//! [`kindboard_core::ProvisionEvent`]) plus a capped monospace output tail.

use std::collections::VecDeque;

use eframe::egui::{self, RichText, ScrollArea};
use kindboard_core::ProvisionEvent;

use crate::bus::{CoreCommand, OpGen};
use crate::theme;

/// Output tail cap for an operation panel.
const OP_OUTPUT_CAP: usize = 400;

/// What kind of operation the panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// Cluster creation.
    Create,
    /// Guided recreate (scale workers).
    Recreate,
    /// Cluster destruction.
    Destroy,
}

impl OpKind {
    fn label(self) -> &'static str {
        match self {
            OpKind::Create => "Create",
            OpKind::Recreate => "Recreate",
            OpKind::Destroy => "Destroy",
        }
    }
}

/// Status of one step in the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// Currently executing.
    Running,
    /// Finished (action + verification).
    Done,
    /// Failed (aborts the run).
    Failed,
}

/// One timeline row.
#[derive(Debug, Clone)]
pub struct StepUi {
    /// Core step id.
    pub id: String,
    /// Display status.
    pub status: StepStatus,
}

/// The panel state for one long-running operation.
#[derive(Debug)]
pub struct OpPanel {
    /// Operation kind.
    pub kind: OpKind,
    /// Cluster name.
    pub name: String,
    /// Generation tag (events with other tags are dropped before reaching
    /// here).
    pub op_gen: OpGen,
    /// Steps in first-seen order.
    pub steps: Vec<StepUi>,
    /// Capped output tail.
    pub output: VecDeque<String>,
    /// `Some` once the operation finished.
    pub finished: Option<Result<(), String>>,
}

impl OpPanel {
    /// Open a panel for a freshly issued operation.
    pub fn start(kind: OpKind, name: impl Into<String>, op_gen: OpGen) -> Self {
        OpPanel {
            kind,
            name: name.into(),
            op_gen,
            steps: Vec::new(),
            output: VecDeque::with_capacity(OP_OUTPUT_CAP),
            finished: None,
        }
    }

    /// Whether the operation is still running.
    pub fn running(&self) -> bool {
        self.finished.is_none()
    }

    fn push_line(&mut self, line: String) {
        if self.output.len() >= OP_OUTPUT_CAP {
            self.output.pop_front();
        }
        self.output.push_back(line);
    }

    fn step_mut(&mut self, id: &str) -> &mut StepUi {
        let position = self
            .steps
            .iter()
            .position(|step| step.id == id)
            .unwrap_or_else(|| {
                self.steps.push(StepUi {
                    id: id.to_string(),
                    status: StepStatus::Running,
                });
                self.steps.len() - 1
            });
        &mut self.steps[position]
    }

    /// Apply one provision event (stale-generation filtering happens in the
    /// app before this is called).
    pub fn apply_event(&mut self, event: &ProvisionEvent) {
        match event {
            ProvisionEvent::StepStarted { id } => {
                let step = self.step_mut(id);
                step.status = StepStatus::Running;
                self.push_line(format!("[{}] started", friendly_step(id)));
            }
            ProvisionEvent::StepOutput { id, line } => {
                self.push_line(format!("[{id}] {line}"));
            }
            ProvisionEvent::StepFinished { id } => {
                let step = self.step_mut(id);
                step.status = StepStatus::Done;
                self.push_line(format!("[{}] finished", friendly_step(id)));
            }
            ProvisionEvent::StepFailed { id, error } => {
                let step = self.step_mut(id);
                step.status = StepStatus::Failed;
                self.push_line(format!("[{}] FAILED: {error}", friendly_step(id)));
            }
            ProvisionEvent::PlanFinished { success } => {
                self.push_line(if *success {
                    "plan finished successfully".to_string()
                } else {
                    "plan aborted".to_string()
                });
            }
        }
    }
}

/// Map a core step id to a friendly timeline label.
pub fn friendly_step(id: &str) -> String {
    if id == "kind-create" {
        return "Create cluster".to_string();
    }
    if id == "merge-kubeconfig" {
        return "Merge kubeconfig".to_string();
    }
    if id == "write-kind-config" {
        return "Write kind config".to_string();
    }
    if id == "destroy-kind" {
        return "Delete cluster".to_string();
    }
    if id == "remove-kubeconfig" {
        return "Remove kubeconfig context".to_string();
    }
    if id == "remove-record" {
        return "Remove stored record".to_string();
    }
    if let Some(rest) = id.strip_prefix("cni-") {
        return format!("CNI: {rest}");
    }
    if let Some(rest) = id.strip_prefix("ingress-") {
        return format!("Ingress: {rest}");
    }
    if let Some(rest) = id.strip_prefix("cilium-") {
        return format!("Cilium extras: {rest}");
    }
    if let Some(rest) = id.strip_prefix("verify-") {
        return format!("Verify: {rest}");
    }
    id.to_string()
}

/// Render an operation window. Returns `true` when the user closed a
/// finished panel (the app removes it).
pub fn show(ctx: &egui::Context, panel: &mut OpPanel, actions: &mut Vec<CoreCommand>) -> bool {
    let mut remove = false;
    let mut open = true;
    let title = format!("{} cluster: {}", panel.kind.label(), panel.name);
    egui::Window::new(title)
        .open(&mut open)
        .resizable(true)
        .default_width(560.0)
        .show(ctx, |ui| {
            // Step timeline.
            if !panel.steps.is_empty() {
                ui.strong("Steps");
                for step in &panel.steps {
                    let (marker, color) = match step.status {
                        StepStatus::Running => ("...", theme::AMBER),
                        StepStatus::Done => ("ok", theme::GREEN),
                        StepStatus::Failed => ("FAILED", theme::RED),
                    };
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("[{marker}]"))
                                .monospace()
                                .color(color)
                                .size(12.0),
                        );
                        ui.label(friendly_step(&step.id));
                    });
                }
                ui.separator();
            }

            // Output tail.
            ui.strong("Output");
            ScrollArea::vertical()
                .stick_to_bottom(true)
                .max_height(220.0)
                .show(ui, |ui| {
                    for line in &panel.output {
                        ui.label(
                            RichText::new(line)
                                .monospace()
                                .size(11.0)
                                .color(theme::TEXT_DIM),
                        );
                    }
                    if panel.running() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("running...").size(11.0));
                        });
                    }
                });

            ui.separator();
            ui.horizontal(|ui| match &panel.finished {
                None => {
                    if ui
                        .button("Cancel")
                        .on_hover_text("Cancel this operation (running steps are terminated)")
                        .clicked()
                    {
                        actions.push(CoreCommand::CancelOp {
                            name: panel.name.clone(),
                        });
                    }
                }
                Some(Ok(())) => {
                    ui.label(RichText::new("Finished").color(theme::GREEN));
                    if ui.button("Close").clicked() {
                        remove = true;
                    }
                }
                Some(Err(err)) => {
                    ui.label(
                        RichText::new(format!("Failed: {err}"))
                            .color(theme::RED)
                            .size(12.0),
                    );
                    if ui.button("Close").clicked() {
                        remove = true;
                    }
                }
            });
        });

    // The window's own close button (X) is equivalent to Close for a
    // finished panel; for a running one it only hides the window.
    if !open {
        if panel.running() {
            actions.push(CoreCommand::CancelOp {
                name: panel.name.clone(),
            });
        } else {
            remove = true;
        }
    }
    remove
}

#[cfg(test)]
mod tests {
    use super::*;
    use kindboard_core::ProvisionEvent;

    #[test]
    fn panel_tracks_steps_in_order() {
        let mut panel = OpPanel::start(OpKind::Create, "demo", 3);
        panel.apply_event(&ProvisionEvent::StepStarted {
            id: "write-kind-config".to_string(),
        });
        panel.apply_event(&ProvisionEvent::StepFinished {
            id: "write-kind-config".to_string(),
        });
        panel.apply_event(&ProvisionEvent::StepStarted {
            id: "kind-create".to_string(),
        });
        assert_eq!(panel.steps.len(), 2);
        assert_eq!(panel.steps[0].id, "write-kind-config");
        assert_eq!(panel.steps[0].status, StepStatus::Done);
        assert_eq!(panel.steps[1].status, StepStatus::Running);
        assert!(panel.running());
    }

    #[test]
    fn panel_finishes_only_via_app() {
        let mut panel = OpPanel::start(OpKind::Destroy, "demo", 1);
        panel.apply_event(&ProvisionEvent::PlanFinished { success: true });
        // The app stamps `finished` from ProvisionDone; plan events alone
        // must not finish the panel.
        assert!(panel.running());
        panel.finished = Some(Ok(()));
        assert!(!panel.running());
    }

    #[test]
    fn output_tail_is_capped() {
        let mut panel = OpPanel::start(OpKind::Create, "demo", 1);
        for index in 0..(super::OP_OUTPUT_CAP + 50) {
            panel.push_line(format!("line {index}"));
        }
        assert_eq!(panel.output.len(), super::OP_OUTPUT_CAP);
        assert_eq!(panel.output.front().map(String::as_str), Some("line 50"));
    }

    #[test]
    fn friendly_step_names() {
        assert_eq!(friendly_step("kind-create"), "Create cluster");
        assert_eq!(
            friendly_step("cni-flannel-download"),
            "CNI: flannel-download"
        );
        assert_eq!(friendly_step("ingress-helm"), "Ingress: helm");
        assert_eq!(friendly_step("cilium-hubble"), "Cilium extras: hubble");
        assert_eq!(friendly_step("mystery-step"), "mystery-step");
    }
}
