//! kube-rs client for live topology reads (ADR-0010).
//!
//! The client is built from the same kubeconfig the app edits, so the app
//! reads exactly what it writes. kube-rs objects are mapped to the stable
//! [`TopologyGraph`](super::TopologyGraph) model at this boundary.

use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::{DaemonSet, Deployment, StatefulSet};
use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::api::core::v1::{Event, Namespace, Node as KNode, Pod, Service, ServiceSpec};
use k8s_openapi::api::networking::v1::Ingress;
use kube::api::{Api, ListParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::{Client, ResourceExt};

use super::{
    Ingress as IngressView, K8sEvent, Namespace as NamespaceView, Node as NodeView, NodeRole,
    OwnerRef, Pod as PodView, Service as ServiceView, ServicePort, TopologyGraph, Workload,
    WorkloadKind,
};
use crate::error::{K8sError, Result};

/// How many recent events to keep in the topology.
pub const MAX_EVENTS: usize = 100;

/// Map a kube-rs error into the crate taxonomy (the two-hop `#[from]` chain
/// does not auto-convert through `CoreError`).
fn kube<T>(result: kube::Result<T>) -> Result<T> {
    result.map_err(K8sError::Kube).map_err(Into::into)
}

/// A kube-rs client bound to one kubeconfig context.
#[derive(Clone)]
pub struct K8sClient {
    client: Client,
    context: String,
}

impl std::fmt::Debug for K8sClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("K8sClient")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

impl K8sClient {
    /// Build a client for the kubeconfig's current context.
    ///
    /// Fails when the kubeconfig has no current context or the context
    /// cannot be resolved.
    pub async fn from_kubeconfig(kubeconfig: Kubeconfig) -> Result<Self> {
        let context = kubeconfig
            .current_context
            .clone()
            .ok_or_else(|| K8sError::Context(String::new(), "no current context".to_string()))?;
        Self::for_context(kubeconfig, context).await
    }

    /// Build a client for an explicit context name.
    pub async fn for_context(kubeconfig: Kubeconfig, context: String) -> Result<Self> {
        let known = kubeconfig.contexts.iter().any(|c| c.name == context);
        if !known {
            return Err(K8sError::Context(
                context.clone(),
                "context not found in kubeconfig".to_string(),
            )
            .into());
        }
        let options = KubeConfigOptions {
            context: Some(context.clone()),
            cluster: None,
            user: None,
        };
        let config = kube::Config::from_custom_kubeconfig(kubeconfig, &options)
            .await
            .map_err(|err| K8sError::Context(context.clone(), err.to_string()))?;
        let client = Client::try_from(config).map_err(K8sError::Kube)?;
        Ok(K8sClient { client, context })
    }

    /// The context this client talks to.
    pub fn context(&self) -> &str {
        &self.context
    }

    /// The underlying kube-rs client (for advanced use).
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Read the full cluster topology into a [`TopologyGraph`].
    pub async fn poll_topology(&self) -> Result<TopologyGraph> {
        let client = &self.client;
        let list_params = ListParams::default();

        let namespaces: Vec<NamespaceView> = kube(
            Api::<Namespace>::all(client.clone())
                .list(&list_params)
                .await,
        )?
        .items
        .into_iter()
        .map(|item| NamespaceView {
            name: item.name_any(),
        })
        .collect();

        let mut workloads = Vec::new();
        workloads.extend(list_workloads::<Deployment>(client, WorkloadKind::Deployment).await?);
        workloads.extend(list_workloads::<StatefulSet>(client, WorkloadKind::StatefulSet).await?);
        workloads.extend(list_workloads::<DaemonSet>(client, WorkloadKind::DaemonSet).await?);
        workloads.extend(list_workloads::<Job>(client, WorkloadKind::Job).await?);
        workloads.extend(list_cronjobs(client).await?);

        let pods: Vec<PodView> = kube(Api::<Pod>::all(client.clone()).list(&list_params).await)?
            .items
            .into_iter()
            .map(pod_view)
            .collect();

        let services: Vec<ServiceView> =
            kube(Api::<Service>::all(client.clone()).list(&list_params).await)?
                .items
                .into_iter()
                .map(service_view)
                .collect();

        let ingresses: Vec<IngressView> =
            kube(Api::<Ingress>::all(client.clone()).list(&list_params).await)?
                .items
                .into_iter()
                .map(ingress_view)
                .collect();

        let nodes: Vec<NodeView> =
            kube(Api::<KNode>::all(client.clone()).list(&list_params).await)?
                .items
                .into_iter()
                .map(node_view)
                .collect();

        let mut events: Vec<K8sEvent> =
            kube(Api::<Event>::all(client.clone()).list(&list_params).await)?
                .items
                .into_iter()
                .map(event_view)
                .collect();
        // Recent events first (best-effort ordering by timestamp; events
        // without timestamps sink to the end).
        events.sort_by_key(|event| std::cmp::Reverse(event.timestamp));
        events.truncate(MAX_EVENTS);

        let layout = super::layout(&TopologyGraph {
            namespaces: namespaces.clone(),
            workloads: workloads.clone(),
            pods: pods.clone(),
            services: services.clone(),
            ingresses: ingresses.clone(),
            nodes: nodes.clone(),
            events: vec![],
            layout: Default::default(),
        });

        Ok(TopologyGraph {
            namespaces,
            workloads,
            pods,
            services,
            ingresses,
            nodes,
            events,
            layout,
        })
    }
}

/// Shared shape of Deployment/StatefulSet/DaemonSet/Job: pod-template
/// selector + template labels.
trait PodTemplateInfo {
    fn selector_and_labels(&self) -> (BTreeMap<String, String>, BTreeMap<String, String>);
}

/// Deployment/StatefulSet/DaemonSet: `spec.selector` is a bare
/// `LabelSelector` (non-optional).
macro_rules! impl_pod_template_info_required_selector {
    ($ty:ty) => {
        impl PodTemplateInfo for $ty {
            fn selector_and_labels(&self) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
                let spec = self.spec.as_ref();
                let selector = spec
                    .and_then(|spec| spec.selector.match_labels.clone())
                    .unwrap_or_default();
                let labels = spec
                    .and_then(|spec| spec.template.metadata.as_ref())
                    .and_then(|metadata| metadata.labels.clone())
                    .unwrap_or_default();
                (selector, labels)
            }
        }
    };
}

impl_pod_template_info_required_selector!(Deployment);
impl_pod_template_info_required_selector!(StatefulSet);
impl_pod_template_info_required_selector!(DaemonSet);

/// Job: `spec.selector` is optional (manualSelector support).
impl PodTemplateInfo for Job {
    fn selector_and_labels(&self) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
        let spec = self.spec.as_ref();
        let selector = spec
            .and_then(|spec| spec.selector.clone())
            .and_then(|selector| selector.match_labels.clone())
            .unwrap_or_default();
        let labels = spec
            .and_then(|spec| spec.template.metadata.as_ref())
            .and_then(|metadata| metadata.labels.clone())
            .unwrap_or_default();
        (selector, labels)
    }
}

async fn list_workloads<K>(client: &Client, kind: WorkloadKind) -> Result<Vec<Workload>>
where
    K: PodTemplateInfo
        + kube::Resource<DynamicType = ()>
        + Clone
        + std::fmt::Debug
        + serde::de::DeserializeOwned,
{
    let items = kube(
        Api::<K>::all(client.clone())
            .list(&ListParams::default())
            .await,
    )?;
    Ok(items
        .items
        .into_iter()
        .map(|item| {
            let (selector, labels) = item.selector_and_labels();
            Workload {
                kind,
                name: item.name_any(),
                ns: item.namespace().unwrap_or_default(),
                selector,
                labels,
            }
        })
        .collect())
}

async fn list_cronjobs(client: &Client) -> Result<Vec<Workload>> {
    let items = kube(
        Api::<CronJob>::all(client.clone())
            .list(&ListParams::default())
            .await,
    )?;
    Ok(items
        .items
        .into_iter()
        .map(|item| {
            let name = item.name_any();
            let ns = item.namespace().unwrap_or_default();
            let selector = item
                .spec
                .job_template
                .spec
                .and_then(|spec| spec.selector.clone())
                .and_then(|selector| selector.match_labels.clone())
                .unwrap_or_default();
            let labels = item
                .spec
                .job_template
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.labels.clone())
                .unwrap_or_default();
            Workload {
                kind: WorkloadKind::CronJob,
                name,
                ns,
                selector,
                labels,
            }
        })
        .collect())
}

fn pod_view(pod: Pod) -> PodView {
    let name = pod.name_any();
    let ns = pod.namespace().unwrap_or_default();
    let spec = pod.spec.unwrap_or_default();
    let status = pod.status.unwrap_or_default();
    let total_containers = spec.containers.len() as u32;
    let ready_containers = status
        .container_statuses
        .unwrap_or_default()
        .iter()
        .filter(|container| container.ready)
        .count() as u32;
    let owner = pod
        .metadata
        .owner_references
        .as_ref()
        .and_then(|refs| refs.first())
        .map(|owner| OwnerRef {
            kind: owner.kind.clone(),
            name: owner.name.clone(),
        });
    PodView {
        name,
        ns,
        owner,
        phase: super::pod_phase_from_str(status.phase.as_deref()),
        node: spec.node_name,
        ready_containers,
        total_containers,
    }
}

fn service_view(service: Service) -> ServiceView {
    let name = service.name_any();
    let ns = service.namespace().unwrap_or_default();
    let spec: ServiceSpec = service.spec.unwrap_or_default();
    ServiceView {
        name,
        ns,
        selector: spec.selector.unwrap_or_default(),
        ports: spec
            .ports
            .unwrap_or_default()
            .into_iter()
            .map(|port| ServicePort {
                name: port.name.unwrap_or_default(),
                port: port.port,
                target_port: port.target_port.map(|target| match target {
                    k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(value) => {
                        value.to_string()
                    }
                    k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(value) => {
                        value
                    }
                }),
                node_port: port.node_port,
                protocol: port.protocol.unwrap_or_else(|| "TCP".to_string()),
            })
            .collect(),
    }
}

fn ingress_view(ingress: Ingress) -> IngressView {
    let name = ingress.name_any();
    let ns = ingress.namespace().unwrap_or_default();
    let spec = ingress.spec.unwrap_or_default();
    let mut hosts = Vec::new();
    let mut backend_services = Vec::new();
    for rule in spec.rules.unwrap_or_default() {
        if let Some(host) = rule.host
            && !hosts.contains(&host)
        {
            hosts.push(host);
        }
        if let Some(http) = rule.http {
            for path in http.paths {
                if let Some(backend) = path.backend.service
                    && !backend_services.contains(&backend.name)
                {
                    backend_services.push(backend.name);
                }
            }
        }
    }
    if let Some(backend) = spec.default_backend.and_then(|b| b.service)
        && !backend_services.contains(&backend.name)
    {
        backend_services.push(backend.name);
    }
    IngressView {
        name,
        ns,
        class: spec.ingress_class_name.unwrap_or_default(),
        backend_services,
        hosts,
    }
}

fn node_view(node: KNode) -> NodeView {
    let metadata = &node.metadata;
    let ready = node
        .status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .map(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        })
        .unwrap_or(false);
    let role = match &metadata.labels {
        Some(labels) if labels.contains_key("node-role.kubernetes.io/control-plane") => {
            NodeRole::ControlPlane
        }
        Some(labels) if labels.contains_key("node-role.kubernetes.io/master") => {
            NodeRole::ControlPlane
        }
        _ => NodeRole::Worker,
    };
    NodeView {
        name: node.name_any(),
        role,
        ready,
    }
}

fn event_view(event: Event) -> K8sEvent {
    let involved = event.involved_object;
    // Prefer first_timestamp, then event_time, then last_timestamp.
    let timestamp = event
        .first_timestamp
        .map(|time| jiff_ts_to_chrono(time.0))
        .or_else(|| event.event_time.map(|time| jiff_ts_to_chrono(time.0)))
        .or_else(|| event.last_timestamp.map(|time| jiff_ts_to_chrono(time.0)))
        .unwrap_or_default();
    K8sEvent {
        ns: involved.namespace.unwrap_or_default(),
        kind: involved.kind.unwrap_or_default(),
        name: involved.name.unwrap_or_default(),
        reason: event.reason.unwrap_or_default(),
        message: event.message.unwrap_or_default(),
        timestamp,
    }
}

/// Convert a jiff timestamp (k8s-openapi 0.28's time representation) to
/// chrono.
fn jiff_ts_to_chrono(timestamp: k8s_openapi::jiff::Timestamp) -> chrono::DateTime<chrono::Utc> {
    let seconds = timestamp.as_second();
    let nanos = timestamp.subsec_nanosecond();
    chrono::DateTime::from_timestamp(seconds, nanos as u32).unwrap_or_default()
}
