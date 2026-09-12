//! Topology data model (contracts §5): the typed view of a cluster the UI
//! renders, plus the deterministic layered layout.
//!
//! The model is *ours* — kube-rs objects are mapped at the boundary so
//! core's types stay stable (ADR-0010).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

/// The full typed view of a cluster at one point in time.
#[derive(Debug, Clone, Default)]
pub struct TopologyGraph {
    /// All namespaces.
    pub namespaces: Vec<Namespace>,
    /// All workloads (deployments/statefulsets/daemonsets/jobs/cronjobs).
    pub workloads: Vec<Workload>,
    /// All pods.
    pub pods: Vec<Pod>,
    /// All services.
    pub services: Vec<Service>,
    /// All ingresses.
    pub ingresses: Vec<Ingress>,
    /// All nodes.
    pub nodes: Vec<Node>,
    /// Recent events (for the detail panel).
    pub events: Vec<K8sEvent>,
    /// Computed rendering positions.
    pub layout: Layout,
}

/// A Kubernetes namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace {
    /// Namespace name.
    pub name: String,
}

/// Which workload kind this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadKind {
    /// Deployment.
    Deployment,
    /// StatefulSet.
    StatefulSet,
    /// DaemonSet.
    DaemonSet,
    /// Job.
    Job,
    /// CronJob (additive over the contracts' 4 kinds; cronjobs are read
    /// and shown but cannot own pods directly).
    CronJob,
}

impl WorkloadKind {
    /// K8s kind name.
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkloadKind::Deployment => "Deployment",
            WorkloadKind::StatefulSet => "StatefulSet",
            WorkloadKind::DaemonSet => "DaemonSet",
            WorkloadKind::Job => "Job",
            WorkloadKind::CronJob => "CronJob",
        }
    }
}

/// A workload (controller of pods).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workload {
    /// Workload kind.
    pub kind: WorkloadKind,
    /// Name.
    pub name: String,
    /// Namespace.
    pub ns: String,
    /// Pod template selector (service→workload edges are computed from
    /// this).
    pub selector: BTreeMap<String, String>,
    /// The workload's own labels (needed to match `service.selector`
    /// against template labels; additive over the contracts so the
    /// documented Service→Workload edge is computable).
    pub labels: BTreeMap<String, String>,
}

/// Reference to the object that owns a pod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRef {
    /// Owner kind (e.g. `ReplicaSet`).
    pub kind: String,
    /// Owner name.
    pub name: String,
}

/// Pod phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PodPhase {
    /// Running (all containers ready or at least one started).
    Running,
    /// Pending (not yet scheduled/started).
    Pending,
    /// Succeeded (completed successfully).
    Succeeded,
    /// Failed.
    Failed,
    /// Unknown (kubelet lost contact or unmapped phase).
    Unknown,
}

/// Map a k8s phase string to [`PodPhase`] (unknown strings → Unknown).
pub fn pod_phase_from_str(phase: Option<&str>) -> PodPhase {
    match phase.unwrap_or("") {
        "Running" => PodPhase::Running,
        "Pending" => PodPhase::Pending,
        "Succeeded" => PodPhase::Succeeded,
        "Failed" => PodPhase::Failed,
        _ => PodPhase::Unknown,
    }
}

/// A pod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pod {
    /// Name.
    pub name: String,
    /// Namespace.
    pub ns: String,
    /// Direct owner (e.g. the ReplicaSet), if any.
    pub owner: Option<OwnerRef>,
    /// Phase.
    pub phase: PodPhase,
    /// Node the pod is scheduled on.
    pub node: Option<String>,
    /// Number of containers reporting ready (additive over the contracts;
    /// drives the green/amber status).
    pub ready_containers: u32,
    /// Total containers in the pod spec (additive; `0` when the spec is
    /// unavailable — never treat as "all ready").
    pub total_containers: u32,
}

/// One port of a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePort {
    /// Port name.
    pub name: String,
    /// Service port.
    pub port: i32,
    /// Target port (pod port or name).
    pub target_port: Option<String>,
    /// Node port (NodePort/LoadBalancer services).
    pub node_port: Option<i32>,
    /// Protocol (TCP/UDP/SCTP).
    pub protocol: String,
}

/// A service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// Name.
    pub name: String,
    /// Namespace.
    pub ns: String,
    /// Selector (Service→Workload edges are derived from this).
    pub selector: BTreeMap<String, String>,
    /// Exposed ports.
    pub ports: Vec<ServicePort>,
}

/// An ingress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingress {
    /// Name.
    pub name: String,
    /// Namespace.
    pub ns: String,
    /// Ingress class.
    pub class: String,
    /// Backend service names (Ingress→Service edges).
    pub backend_services: Vec<String>,
    /// Hosts.
    pub hosts: Vec<String>,
}

/// Node role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    /// Control-plane node.
    ControlPlane,
    /// Worker node.
    Worker,
}

/// A cluster node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Name.
    pub name: String,
    /// Role.
    pub role: NodeRole,
    /// Ready condition.
    pub ready: bool,
}

/// A recent Kubernetes event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct K8sEvent {
    /// Namespace of the involved object.
    pub ns: String,
    /// Kind of the involved object.
    pub kind: String,
    /// Name of the involved object.
    pub name: String,
    /// Reason (short machine string).
    pub reason: String,
    /// Human-readable message.
    pub message: String,
    /// Best-known timestamp (first observation preferred).
    pub timestamp: DateTime<Utc>,
}

/// One laid-out node (position for the painter).
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutNode {
    /// Stable id (`<kind>/<ns>/<name>`).
    pub id: String,
    /// Kind of the node (drives the painter's shape).
    pub kind: LayoutKind,
    /// Name to display.
    pub name: String,
    /// X position.
    pub x: f32,
    /// Y position.
    pub y: f32,
}

/// What kind of object a layout node represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKind {
    /// Namespace.
    Namespace,
    /// Workload.
    Workload,
    /// Pod.
    Pod,
    /// Service.
    Service,
    /// Ingress.
    Ingress,
}

/// Computed positions for a [`TopologyGraph`].
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// Nodes with positions.
    pub nodes: Vec<LayoutNode>,
}

/// Vertical spacing between layers.
pub const LAYER_SPACING: f32 = 140.0;
/// Horizontal spacing between nodes in a layer.
pub const NODE_SPACING: f32 = 180.0;

/// Layer rank per kind (contracts §5: namespaces 0 → workloads 1 → pods 2 →
/// services 3 → ingresses 4).
pub fn layer_rank(kind: LayoutKind) -> usize {
    match kind {
        LayoutKind::Namespace => 0,
        LayoutKind::Workload => 1,
        LayoutKind::Pod => 2,
        LayoutKind::Service => 3,
        LayoutKind::Ingress => 4,
    }
}

/// Compute a deterministic layered layout for a graph.
///
/// Nodes within a layer are ordered by name (stable), then one Sugiyama
/// barycenter pass reorders services by the average position of the
/// workloads they select (minimizing edge crossings).
pub fn layout(graph: &TopologyGraph) -> Layout {
    let mut layout = Layout::default();
    let mut buckets: Vec<Vec<LayoutNode>> = vec![Vec::new(); 5];

    for ns in &graph.namespaces {
        buckets[0].push(node(LayoutKind::Namespace, "", &ns.name));
    }
    for workload in &graph.workloads {
        buckets[1].push(node(LayoutKind::Workload, &workload.ns, &workload.name));
    }
    for pod in &graph.pods {
        buckets[2].push(node(LayoutKind::Pod, &pod.ns, &pod.name));
    }
    for service in &graph.services {
        buckets[3].push(node(LayoutKind::Service, &service.ns, &service.name));
    }
    for ingress in &graph.ingresses {
        buckets[4].push(node(LayoutKind::Ingress, &ingress.ns, &ingress.name));
    }
    for bucket in &mut buckets {
        bucket.sort_by(|a, b| a.name.cmp(&b.name));
    }

    // Barycenter pass: order services by the mean index of the workloads
    // their selector matches (closer services sit nearer their backends).
    let workload_positions: BTreeMap<String, usize> = buckets[1]
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.clone(), index))
        .collect();
    let service_order: Vec<(f32, String)> = graph
        .services
        .iter()
        .map(|service| {
            let positions: Vec<usize> = graph
                .workloads
                .iter()
                .filter(|workload| {
                    workload.ns == service.ns
                        && selector_matches(&service.selector, &workload.labels)
                })
                .filter_map(|workload| {
                    let id = workload_id(workload);
                    workload_positions.get(&id).copied()
                })
                .collect();
            let mean = if positions.is_empty() {
                f32::MAX // unconnected services sink to the bottom
            } else {
                positions.iter().sum::<usize>() as f32 / positions.len() as f32
            };
            (mean, service_id(service))
        })
        .collect::<Vec<_>>();
    if !service_order.is_empty() {
        let mut service_map: BTreeMap<String, LayoutNode> = buckets[3]
            .drain(..)
            .map(|node| (node.id.clone(), node))
            .collect();
        let mut ordered: Vec<LayoutNode> = Vec::with_capacity(service_order.len());
        let mut order = service_order.clone();
        order.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for (_, id) in order {
            if let Some(node) = service_map.remove(&id) {
                ordered.push(node);
            }
        }
        // Any leftovers (shouldn't happen) keep name order.
        let mut leftovers: Vec<LayoutNode> = service_map.into_values().collect();
        leftovers.sort_by(|a, b| a.name.cmp(&b.name));
        ordered.extend(leftovers);
        buckets[3] = ordered;
    }

    for (rank, bucket) in buckets.into_iter().enumerate() {
        let y = rank as f32 * LAYER_SPACING;
        for (index, mut node) in bucket.into_iter().enumerate() {
            node.x = index as f32 * NODE_SPACING;
            node.y = y;
            layout.nodes.push(node);
        }
    }
    layout
}

fn node(kind: LayoutKind, ns: &str, name: &str) -> LayoutNode {
    LayoutNode {
        id: layout_id(kind, ns, name),
        kind,
        name: name.to_string(),
        x: 0.0,
        y: 0.0,
    }
}

fn layout_id(kind: LayoutKind, ns: &str, name: &str) -> String {
    let kind_str = match kind {
        LayoutKind::Namespace => "namespace",
        LayoutKind::Workload => "workload",
        LayoutKind::Pod => "pod",
        LayoutKind::Service => "service",
        LayoutKind::Ingress => "ingress",
    };
    if ns.is_empty() {
        format!("{kind_str}/{name}")
    } else {
        format!("{kind_str}/{ns}/{name}")
    }
}

fn service_id(service: &Service) -> String {
    layout_id(LayoutKind::Service, &service.ns, &service.name)
}

fn workload_id(workload: &Workload) -> String {
    layout_id(LayoutKind::Workload, &workload.ns, &workload.name)
}

/// Whether a label selector matches a label set (equality-only selectors).
pub fn selector_matches(
    selector: &BTreeMap<String, String>,
    labels: &BTreeMap<String, String>,
) -> bool {
    if selector.is_empty() {
        return false;
    }
    selector
        .iter()
        .all(|(key, value)| labels.get(key) == Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_graph() -> TopologyGraph {
        TopologyGraph {
            namespaces: vec![
                Namespace {
                    name: "default".to_string(),
                },
                Namespace {
                    name: "kube-system".to_string(),
                },
            ],
            workloads: vec![
                Workload {
                    kind: WorkloadKind::Deployment,
                    name: "web".to_string(),
                    ns: "default".to_string(),
                    selector: BTreeMap::from([("app".to_string(), "web".to_string())]),
                    labels: BTreeMap::from([("app".to_string(), "web".to_string())]),
                },
                Workload {
                    kind: WorkloadKind::Deployment,
                    name: "api".to_string(),
                    ns: "default".to_string(),
                    selector: BTreeMap::from([("app".to_string(), "api".to_string())]),
                    labels: BTreeMap::from([("app".to_string(), "api".to_string())]),
                },
            ],
            pods: vec![
                Pod {
                    name: "web-0".to_string(),
                    ns: "default".to_string(),
                    owner: Some(OwnerRef {
                        kind: "ReplicaSet".to_string(),
                        name: "web-abc".to_string(),
                    }),
                    phase: PodPhase::Running,
                    node: Some("demo-control-plane".to_string()),
                    ready_containers: 1,
                    total_containers: 1,
                },
                Pod {
                    name: "api-0".to_string(),
                    ns: "default".to_string(),
                    owner: None,
                    phase: PodPhase::Pending,
                    node: None,
                    ready_containers: 0,
                    total_containers: 2,
                },
            ],
            services: vec![
                Service {
                    name: "web-svc".to_string(),
                    ns: "default".to_string(),
                    selector: BTreeMap::from([("app".to_string(), "web".to_string())]),
                    ports: vec![ServicePort {
                        name: "http".to_string(),
                        port: 80,
                        target_port: Some("8080".to_string()),
                        node_port: None,
                        protocol: "TCP".to_string(),
                    }],
                },
                Service {
                    name: "api-svc".to_string(),
                    ns: "default".to_string(),
                    selector: BTreeMap::from([("app".to_string(), "api".to_string())]),
                    ports: vec![],
                },
            ],
            ingresses: vec![Ingress {
                name: "web-ing".to_string(),
                ns: "default".to_string(),
                class: "nginx".to_string(),
                backend_services: vec!["web-svc".to_string()],
                hosts: vec!["example.test".to_string()],
            }],
            nodes: vec![
                Node {
                    name: "demo-control-plane".to_string(),
                    role: NodeRole::ControlPlane,
                    ready: true,
                },
                Node {
                    name: "demo-worker".to_string(),
                    role: NodeRole::Worker,
                    ready: false,
                },
            ],
            events: vec![],
            layout: Layout::default(),
        }
    }

    #[test]
    fn layer_ranks_are_ordered() {
        assert!(layer_rank(LayoutKind::Namespace) < layer_rank(LayoutKind::Workload));
        assert!(layer_rank(LayoutKind::Workload) < layer_rank(LayoutKind::Pod));
        assert!(layer_rank(LayoutKind::Pod) < layer_rank(LayoutKind::Service));
        assert!(layer_rank(LayoutKind::Service) < layer_rank(LayoutKind::Ingress));
    }

    #[test]
    fn layout_is_deterministic_and_layered() {
        let graph = sample_graph();
        let a = layout(&graph);
        let b = layout(&graph);
        assert_eq!(a.nodes, b.nodes, "layout must be deterministic");

        // The layered layout covers namespaces..ingresses (5 ranks); cluster
        // nodes are not part of the layered DAG (contracts §5).
        assert_eq!(a.nodes.len(), 9);
        let namespaces: Vec<&LayoutNode> = a
            .nodes
            .iter()
            .filter(|n| n.kind == LayoutKind::Namespace)
            .collect();
        assert_eq!(namespaces[0].name, "default");
        assert_eq!(namespaces[1].name, "kube-system");
        assert_eq!(namespaces[0].y, 0.0);

        let workloads: Vec<&LayoutNode> = a
            .nodes
            .iter()
            .filter(|n| n.kind == LayoutKind::Workload)
            .collect();
        assert_eq!(workloads[0].name, "api");
        assert_eq!(workloads[1].name, "web");
        assert_eq!(workloads[0].y, LAYER_SPACING);

        // Services ordered by barycenter: web-svc selects "web" (index 1),
        // api-svc selects "api" (index 0) → api-svc first.
        let services: Vec<&LayoutNode> = a
            .nodes
            .iter()
            .filter(|n| n.kind == LayoutKind::Service)
            .collect();
        assert_eq!(services[0].name, "api-svc");
        assert_eq!(services[1].name, "web-svc");

        let ingresses: Vec<&LayoutNode> = a
            .nodes
            .iter()
            .filter(|n| n.kind == LayoutKind::Ingress)
            .collect();
        assert_eq!(ingresses.len(), 1);
        assert_eq!(ingresses[0].y, 4.0 * LAYER_SPACING);
    }

    #[test]
    fn selector_matching_is_equality_only() {
        let selector = BTreeMap::from([("app".to_string(), "web".to_string())]);
        let matching = BTreeMap::from([
            ("app".to_string(), "web".to_string()),
            ("tier".to_string(), "frontend".to_string()),
        ]);
        let wrong_value = BTreeMap::from([("app".to_string(), "api".to_string())]);
        assert!(selector_matches(&selector, &matching));
        assert!(!selector_matches(&selector, &wrong_value));
        assert!(!selector_matches(&BTreeMap::new(), &matching));
        assert!(!selector_matches(&selector, &BTreeMap::new()));
    }

    #[test]
    fn pod_phase_mapping_covers_all_and_unknown() {
        assert_eq!(pod_phase_from_str(Some("Running")), PodPhase::Running);
        assert_eq!(pod_phase_from_str(Some("Pending")), PodPhase::Pending);
        assert_eq!(pod_phase_from_str(Some("Succeeded")), PodPhase::Succeeded);
        assert_eq!(pod_phase_from_str(Some("Failed")), PodPhase::Failed);
        assert_eq!(pod_phase_from_str(Some("Bogus")), PodPhase::Unknown);
        assert_eq!(pod_phase_from_str(None), PodPhase::Unknown);
    }

    #[test]
    fn workload_kind_names() {
        assert_eq!(WorkloadKind::Deployment.as_str(), "Deployment");
        assert_eq!(WorkloadKind::CronJob.as_str(), "CronJob");
    }
}
