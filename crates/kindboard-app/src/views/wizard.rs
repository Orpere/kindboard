//! Create-cluster wizard: modal form with on-the-fly validation through
//! `kindboard_core::spec::validate`. The Create button stays disabled until
//! the spec is valid; every invalid field is explained by the core error.

use std::collections::BTreeMap;

use eframe::egui::{self, Modal, RichText};
use kindboard_core::{
    CiliumOptions, ClusterSpec, Cni, IngressController, KubernetesVersion, PortMapping, Protocol,
    validate, validate_name,
};

use crate::theme;
use crate::util::inline_error;

/// Form state of the wizard.
pub struct WizardState {
    /// Editable fields.
    pub name: String,
    pub k8s_version: String,
    pub cni: Cni,
    pub pod_cidr: String,
    pub service_cidr: String,
    pub worker_count: u32,
    pub ingress: Option<IngressController>,
    pub cilium: CiliumOptions,
}

impl WizardState {
    /// A fresh form seeded from core defaults.
    pub fn fresh() -> Self {
        WizardState {
            name: String::new(),
            k8s_version: kindboard_core::DEFAULT_K8S_VERSION.to_string(),
            cni: Cni::KindnetDefault,
            pod_cidr: kindboard_core::DEFAULT_POD_CIDR.to_string(),
            service_cidr: kindboard_core::DEFAULT_SERVICE_CIDR.to_string(),
            worker_count: 0,
            ingress: None,
            cilium: CiliumOptions::default(),
        }
    }

    /// Pre-fill from an existing spec (recreate-from-record entry point).
    pub fn from_spec(spec: &ClusterSpec) -> Self {
        WizardState {
            name: spec.name.clone(),
            k8s_version: spec.k8s_version.as_str().to_string(),
            cni: spec.cni,
            pod_cidr: spec.pod_cidr.clone(),
            service_cidr: spec.service_cidr.clone(),
            worker_count: spec.worker_count,
            ingress: spec.ingress,
            cilium: spec.cilium.clone().unwrap_or_default(),
        }
    }

    /// Build the spec from the form (unvalidated; callers run
    /// [`validate`]).
    pub fn to_spec(&self) -> ClusterSpec {
        let cni = self.cni;
        let cilium = (cni == Cni::Cilium).then(|| self.cilium.clone());
        let ingress = self.ingress;
        // Contracts rule: host ports 80/443 are auto-managed with the
        // nginx/traefik ingress choice (and only then).
        let extra_port_mappings = match ingress {
            Some(IngressController::Nginx | IngressController::Traefik) => vec![
                PortMapping {
                    container_port: 80,
                    host_port: 80,
                    listen_address: "127.0.0.1".to_string(),
                    protocol: Protocol::Tcp,
                },
                PortMapping {
                    container_port: 443,
                    host_port: 443,
                    listen_address: "127.0.0.1".to_string(),
                    protocol: Protocol::Tcp,
                },
            ],
            _ => Vec::new(),
        };
        ClusterSpec {
            name: self.name.trim().to_string(),
            // Fall back to the (always-valid) default version when the
            // field is unparseable; `validate` below still gates creation.
            k8s_version: KubernetesVersion::new(self.k8s_version.trim())
                .unwrap_or_else(|_| KubernetesVersion::default()),
            cni,
            pod_cidr: self.pod_cidr.trim().to_string(),
            service_cidr: self.service_cidr.trim().to_string(),
            worker_count: self.worker_count,
            extra_port_mappings,
            feature_gates: BTreeMap::new(),
            ingress,
            cilium,
        }
    }
}

/// Render the wizard modal. Returns `Some(spec)` when the user confirmed a
/// valid spec, `None` while it stays open, and `WizardAction::Cancel` when
/// dismissed.
pub enum WizardAction {
    /// Keep the wizard open.
    Stay,
    /// User confirmed: carry the validated spec.
    Create(ClusterSpec),
    /// User cancelled.
    Cancel,
}

#[allow(clippy::too_many_lines)] // a form with 10 fields + 2 inline groups; the
// fields share state and validation so splitting would scatter it.
pub fn show(ctx: &egui::Context, state: &mut WizardState) -> WizardAction {
    let mut action = WizardAction::Stay;
    let modal = Modal::new(egui::Id::new("create-wizard")).show(ctx, |ui| {
        // Clamp to the viewport: the modal must really fit whatever the
        // window size is (R3), so the form scrolls on short screens and
        // shrinks on narrow ones.
        ui.set_width(ui.available_width().min(520.0));
        ui.heading("Create cluster");
        ui.add_space(4.0);

        let body_height = (ui.available_height() - 90.0).max(120.0);
        egui::ScrollArea::vertical()
            .id_salt("wizard-scroll")
            .max_height(body_height)
            .show(ui, |ui| {
                ui.label("Name");
                ui.add(
                    egui::TextEdit::singleline(&mut state.name)
                        .hint_text("my-cluster (lowercase, digits, dashes)")
                        .desired_width(f32::INFINITY),
                );
                if !state.name.is_empty()
                    && let Err(err) = validate_name(&state.name)
                {
                    inline_error(ui, &err.to_string());
                }

                ui.label("Kubernetes version");
                ui.add(
                    egui::TextEdit::singleline(&mut state.k8s_version)
                        .hint_text("1.37.0")
                        .desired_width(f32::INFINITY),
                );

                ui.add_space(4.0);
                ui.strong("CNI");
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut state.cni, Cni::KindnetDefault, "Kindnet (default)");
                    ui.radio_value(&mut state.cni, Cni::Flannel, "Flannel");
                    ui.radio_value(&mut state.cni, Cni::Calico, "Calico");
                    ui.radio_value(&mut state.cni, Cni::Cilium, "Cilium");
                });

                ui.add_space(4.0);
                ui.strong("Network CIDRs");
                ui.horizontal_wrapped(|ui| {
                    ui.label("pod:");
                    ui.add(
                        egui::TextEdit::singleline(&mut state.pod_cidr)
                            .hint_text(kindboard_core::DEFAULT_POD_CIDR)
                            .desired_width(160.0),
                    );
                    ui.label("service:");
                    ui.add(
                        egui::TextEdit::singleline(&mut state.service_cidr)
                            .hint_text(kindboard_core::DEFAULT_SERVICE_CIDR)
                            .desired_width(160.0),
                    );
                });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.strong("Workers");
                    ui.add(
                        egui::DragValue::new(&mut state.worker_count)
                            .range(0..=kindboard_core::MAX_WORKERS)
                            .suffix(" nodes"),
                    );
                    ui.label(
                        RichText::new("(1 control-plane is always added)")
                            .color(theme::pal().text_dim)
                            .size(11.0),
                    );
                });

                ui.add_space(4.0);
                ui.strong("Ingress controller");
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut state.ingress, None, "None");
                    ui.radio_value(&mut state.ingress, Some(IngressController::Nginx), "Nginx");
                    ui.radio_value(
                        &mut state.ingress,
                        Some(IngressController::Traefik),
                        "Traefik",
                    );
                    if state.cni == Cni::Cilium {
                        ui.radio_value(
                            &mut state.ingress,
                            Some(IngressController::Cilium),
                            "Cilium",
                        );
                    }
                });
                if state.ingress == Some(IngressController::Cilium) && !state.cilium.ingress {
                    inline_error(
                        ui,
                        "Cilium ingress requires the 'Ingress Controller' cilium extra below",
                    );
                }
                match state.ingress {
                    Some(IngressController::Nginx | IngressController::Traefik) => {
                        ui.label(
                            RichText::new(
                                "host ports 80/443 -> node ports are mapped automatically",
                            )
                            .color(theme::pal().text_dim)
                            .size(11.0),
                        );
                    }
                    None if state.cni == Cni::Cilium && state.cilium.ingress => {
                        ui.label(
                            RichText::new(
                                "no host ports mapped; the Cilium ingress controller is installed by default (see Cilium extras)",
                            )
                            .color(theme::pal().text_dim)
                            .size(11.0),
                        );
                    }
                    _ => {
                        ui.label(
                            RichText::new(
                                "no host ports mapped (Cilium ingress uses a LoadBalancer service)",
                            )
                            .color(theme::pal().text_dim)
                            .size(11.0),
                        );
                    }
                }

                if state.cni == Cni::Cilium {
                    ui.add_space(4.0);
                    ui.strong("Cilium extras");
                    ui.checkbox(&mut state.cilium.api_gateway, "API Gateway");
                    ui.checkbox(&mut state.cilium.hubble, "Hubble (relay + UI)");
                    ui.checkbox(&mut state.cilium.ingress, "Ingress Controller");
                    ui.checkbox(&mut state.cilium.mesh, "Clustermesh");
                    if state.cilium.mesh {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("cluster id:");
                            ui.add(
                                egui::DragValue::new(&mut state.cilium.cluster_id).range(
                                    kindboard_core::spec::MIN_CLUSTER_ID
                                        ..=kindboard_core::spec::MAX_CLUSTER_ID,
                                ),
                            );
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("cluster name:");
                            ui.add(
                                egui::TextEdit::singleline(&mut state.cilium.cluster_name)
                                    .hint_text("lowercase, <= 32 chars")
                                    .desired_width(220.0),
                            );
                        });
                    }
                }
            });

        ui.add_space(8.0);
        ui.separator();

        // On-the-fly validation: core is the single source of truth.
        let spec = state.to_spec();
        match validate(&spec) {
            Ok(()) => {
                ui.label(RichText::new("Spec OK").color(theme::pal().green).size(12.0));
            }
            Err(err) => {
                inline_error(ui, &err.to_string());
            }
        }

        ui.add_space(8.0);
        let valid = validate(&spec).is_ok();
        ui.horizontal(|ui| {
            let create = ui.add_enabled(
                valid && !state.name.is_empty(),
                egui::Button::new(RichText::new("Create").strong())
                    .min_size(egui::vec2(110.0, 30.0)),
            );
            if create
                .on_hover_text("Create the cluster (streams progress into an operation panel)")
                .clicked()
            {
                action = WizardAction::Create(spec);
            }
            if ui.button("Cancel").clicked() {
                action = WizardAction::Cancel;
            }
        });
    });

    if modal.should_close() {
        action = WizardAction::Cancel;
    }
    if modal.backdrop_response.clicked() {
        action = WizardAction::Cancel;
    }
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use kindboard_core::Protocol;

    fn form() -> WizardState {
        let mut state = WizardState::fresh();
        state.name = "demo".to_string();
        state
    }

    #[test]
    fn nginx_ingress_adds_host_ports_80_443() {
        let mut state = form();
        state.ingress = Some(IngressController::Nginx);
        let spec = state.to_spec();
        assert_eq!(spec.extra_port_mappings.len(), 2);
        assert_eq!(spec.extra_port_mappings[0].host_port, 80);
        assert_eq!(spec.extra_port_mappings[1].host_port, 443);
        assert_eq!(spec.extra_port_mappings[0].protocol, Protocol::Tcp);
    }

    #[test]
    fn cilium_ingress_has_no_host_ports() {
        let mut state = form();
        state.cni = Cni::Cilium;
        state.cilium.ingress = true;
        state.ingress = Some(IngressController::Cilium);
        let spec = state.to_spec();
        assert!(spec.extra_port_mappings.is_empty());
    }

    #[test]
    fn cilium_options_only_when_cni_is_cilium() {
        let mut state = form();
        state.cni = Cni::KindnetDefault;
        state.cilium.ingress = true;
        assert!(state.to_spec().cilium.is_none());

        state.cni = Cni::Cilium;
        assert!(state.to_spec().cilium.is_some());
    }

    #[test]
    fn fresh_form_is_valid_save_name() {
        let mut state = form();
        state.worker_count = 2;
        let spec = state.to_spec();
        assert!(kindboard_core::validate(&spec).is_ok());
    }

    #[test]
    fn invalid_names_fail_validation() {
        let mut state = form();
        state.name = "Bad_Name!".to_string();
        let spec = state.to_spec();
        assert!(kindboard_core::validate(&spec).is_err());
    }
}
