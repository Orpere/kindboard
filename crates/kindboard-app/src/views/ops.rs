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
    /// Whether to use kind-style progress formatting.
    pub create_style: bool,
    /// Whether the header line has already been printed.
    pub header_printed: bool,
    /// Index into `output` of the in-progress "…" line, if any.
    pub pending_step: Option<usize>,
}

impl OpPanel {
    /// Open a panel for a freshly issued operation.
    pub fn start(kind: OpKind, name: impl Into<String>, op_gen: OpGen) -> Self {
        let name = name.into();
        let create_style = matches!(kind, OpKind::Create | OpKind::Recreate);
        OpPanel {
            kind,
            name,
            op_gen,
            steps: Vec::new(),
            output: VecDeque::with_capacity(OP_OUTPUT_CAP),
            finished: None,
            create_style,
            header_printed: false,
            pending_step: None,
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
                if self.create_style {
                    if !self.header_printed {
                        let verb = match self.kind {
                            OpKind::Create => "Creating",
                            OpKind::Recreate => "Recreating",
                            OpKind::Destroy => unreachable!("create_style excludes Destroy"),
                        };
                        self.push_line(format!("{verb} cluster \"{}\" ...", self.name));
                        self.header_printed = true;
                    }
                    let index = self.output.len();
                    self.push_line(format!("   {} ...", friendly_step(id)));
                    self.pending_step = Some(index);
                } else {
                    self.push_line(format!("[{}] started", friendly_step(id)));
                }
            }
            ProvisionEvent::StepOutput { id, line } => {
                self.push_line(format!("[{}] {line}", friendly_step(id)));
            }
            ProvisionEvent::StepFinished { id } => {
                let step = self.step_mut(id);
                step.status = StepStatus::Done;
                if self.create_style {
                    let line = format!(" \u{2713} {}", friendly_step(id));
                    if let Some(index) = self
                        .pending_step
                        .take()
                        .filter(|index| *index < self.output.len())
                    {
                        self.output[index] = line;
                    } else {
                        self.push_line(line);
                    }
                } else {
                    self.push_line(format!("[{}] finished", friendly_step(id)));
                }
            }
            ProvisionEvent::StepFailed { id, error } => {
                let step = self.step_mut(id);
                step.status = StepStatus::Failed;
                if self.create_style {
                    let line = format!(" \u{2717} {}", friendly_step(id));
                    if let Some(index) = self
                        .pending_step
                        .take()
                        .filter(|index| *index < self.output.len())
                    {
                        self.output[index] = line;
                    } else {
                        self.push_line(line);
                    }
                    self.push_line(format!("  {error}"));
                } else {
                    self.push_line(format!("[{}] FAILED: {error}", friendly_step(id)));
                }
            }
            ProvisionEvent::PlanFinished { success } => {
                if self.create_style {
                    if *success {
                        self.push_line(format!("Set kubectl context to \"kind-{}\"", self.name));
                        self.push_line("You can now use your cluster with:".to_string());
                        self.push_line(String::new());
                        self.push_line(format!(
                            "kubectl cluster-info --context kind-{}",
                            self.name
                        ));
                    } else {
                        self.push_line(" \u{2717} plan aborted".to_string());
                    }
                } else {
                    self.push_line(if *success {
                        "plan finished successfully".to_string()
                    } else {
                        "plan aborted".to_string()
                    });
                }
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
                        StepStatus::Running => ("\u{2026}", theme::AMBER),
                        StepStatus::Done => ("\u{2713}", theme::GREEN),
                        StepStatus::Failed => ("\u{2717}", theme::RED),
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

    #[test]
    fn styled_create_prints_kind_like_progress() {
        let mut panel = OpPanel::start(OpKind::Create, "demo", 1);
        panel.apply_event(&ProvisionEvent::StepStarted {
            id: "kind-create".to_string(),
        });
        panel.apply_event(&ProvisionEvent::StepOutput {
            id: "cni-cilium-install".to_string(),
            line: "some line".to_string(),
        });
        panel.apply_event(&ProvisionEvent::StepFinished {
            id: "kind-create".to_string(),
        });
        panel.apply_event(&ProvisionEvent::StepStarted {
            id: "cni-cilium-install".to_string(),
        });
        panel.apply_event(&ProvisionEvent::StepFailed {
            id: "cni-cilium-install".to_string(),
            error: "boom".to_string(),
        });
        panel.apply_event(&ProvisionEvent::PlanFinished { success: false });
        let lines: Vec<&str> = panel.output.iter().map(String::as_str).collect();
        assert_eq!(
            lines,
            vec![
                "Creating cluster \"demo\" ...",
                " \u{2713} Create cluster",
                "[CNI: cilium-install] some line",
                " \u{2717} CNI: cilium-install",
                "  boom",
                " \u{2717} plan aborted",
            ]
        );
    }

    #[test]
    fn styled_create_success_footer() {
        let mut panel = OpPanel::start(OpKind::Create, "demo", 1);
        panel.apply_event(&ProvisionEvent::PlanFinished { success: true });
        let tail: Vec<String> = panel
            .output
            .iter()
            .skip(panel.output.len().saturating_sub(4))
            .cloned()
            .collect();
        assert_eq!(
            tail,
            vec![
                "Set kubectl context to \"kind-demo\"",
                "You can now use your cluster with:",
                "",
                "kubectl cluster-info --context kind-demo",
            ]
        );
    }

    #[test]
    fn destroy_panel_keeps_legacy_format() {
        let mut panel = OpPanel::start(OpKind::Destroy, "demo", 1);
        panel.apply_event(&ProvisionEvent::StepStarted {
            id: "merge-kubeconfig".to_string(),
        });
        panel.apply_event(&ProvisionEvent::StepFinished {
            id: "merge-kubeconfig".to_string(),
        });
        let lines: Vec<&str> = panel.output.iter().map(String::as_str).collect();
        assert_eq!(
            lines,
            vec!["[Merge kubeconfig] started", "[Merge kubeconfig] finished"]
        );
    }

    #[test]
    fn step_output_uses_friendly_prefix() {
        let mut panel = OpPanel::start(OpKind::Create, "demo", 1);
        panel.apply_event(&ProvisionEvent::StepOutput {
            id: "cni-flannel-install".to_string(),
            line: "x".to_string(),
        });
        let lines: Vec<&str> = panel.output.iter().map(String::as_str).collect();
        assert_eq!(lines, vec!["[CNI: flannel-install] x"]);
    }

    #[test]
    fn styled_header_printed_only_once() {
        let mut panel = OpPanel::start(OpKind::Recreate, "demo", 1);
        panel.apply_event(&ProvisionEvent::StepStarted {
            id: "kind-create".to_string(),
        });
        panel.apply_event(&ProvisionEvent::StepStarted {
            id: "cni-cilium-install".to_string(),
        });
        assert_eq!(
            panel
                .output
                .iter()
                .filter(|l| l.starts_with("Recreating"))
                .count(),
            1
        );
        assert_eq!(
            panel.output.front().map(String::as_str),
            Some("Recreating cluster \"demo\" ...")
        );
    }
}
