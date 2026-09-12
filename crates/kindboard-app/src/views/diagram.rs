//! Topology diagram painter.
//!
//! Consumes the deterministic layered layout computed by
//! `kindboard_core::k8s::layout` (namespaces → workloads → pods → services
//! → ingresses, barycenter-ordered services) and draws it with pan/zoom.
//! Edge rules mirror contracts §5:
//! - Service → Workload: `selector_matches(service.selector, workload.labels)`
//!   within the same namespace (core helper).
//! - Workload → Pod: direct owner (`owner.name == workload.name` or the
//!   ReplicaSet-style prefix `<workload>-`).
//! - Ingress → Service: `backend_services` entry within the same namespace.
//!
//! Status colors: Running/Ready green, Pending amber, Failed red, Unknown
//! grey. The module is a pure painter: no I/O, no core logic.

use std::collections::{BTreeMap, HashSet};

use eframe::egui::{self, Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};
use kindboard_core::{K8sEvent, LayoutKind, PodPhase, Service, TopologyGraph, Workload};

use crate::theme;
use crate::util::{time_of, truncate};

/// Pan/zoom state of one cluster's diagram (persists across polls).
#[derive(Debug, Clone, Default)]
pub struct DiagramState {
    /// Pan offset in screen pixels.
    pub offset: Vec2,
    /// Zoom factor (0.2..=3.0).
    pub zoom: f32,
    /// Fit-to-view on the next frame.
    pub fit_next: bool,
}

impl DiagramState {
    /// Fresh state; the first draw fits the view.
    pub fn new() -> Self {
        DiagramState {
            offset: Vec2::ZERO,
            zoom: 1.0,
            fit_next: true,
        }
    }
}

/// Node draw metrics.
const NS_W: f32 = 150.0;
const NS_H: f32 = 30.0;
const WK_W: f32 = 140.0;
const WK_H: f32 = 30.0;
const POD_W: f32 = 130.0;
const POD_H: f32 = 26.0;
const SVC_W: f32 = 130.0;
const SVC_H: f32 = 26.0;
const ING_W: f32 = 130.0;
const ING_H: f32 = 26.0;
const MIN_ZOOM: f32 = 0.2;
const MAX_ZOOM: f32 = 3.0;

/// One diagram node resolved against the graph model.
struct ViewNode {
    layout: kindboard_core::k8s::LayoutNode,
    /// Namespace this node belongs to ("" for namespace nodes).
    ns: String,
    status: NodeStatus,
    /// Short kind glyph for workload labels ("D", "SS", "DS", "J", "CJ").
    glyph: Option<&'static str>,
}

/// Status classification used for coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeStatus {
    Ready,
    Pending,
    Failed,
    Succeeded,
    Neutral,
}

impl NodeStatus {
    fn color(self) -> Color32 {
        match self {
            NodeStatus::Ready => theme::GREEN,
            NodeStatus::Pending => theme::AMBER,
            NodeStatus::Failed => theme::RED,
            NodeStatus::Succeeded => Color32::from_rgb(0x4f, 0x9d, 0xc9),
            NodeStatus::Neutral => theme::TEXT_DIM,
        }
    }
}

/// Aggregate pod-driven status of a workload.
fn workload_status(workload: &Workload, pods: &[kindboard_core::Pod]) -> NodeStatus {
    let owned: Vec<&kindboard_core::Pod> = pods
        .iter()
        .filter(|pod| pod_owned_by(pod, workload))
        .collect();
    if owned.is_empty() {
        return NodeStatus::Neutral;
    }
    if owned.iter().any(|pod| pod.phase == PodPhase::Failed) {
        return NodeStatus::Failed;
    }
    let all_ready = owned.iter().all(|pod| {
        pod.phase == PodPhase::Running
            && pod.total_containers > 0
            && pod.ready_containers == pod.total_containers
    });
    if all_ready {
        NodeStatus::Ready
    } else {
        NodeStatus::Pending
    }
}

/// Whether a pod is directly owned by a workload (owner name exact match or
/// ReplicaSet-style `<workload>-<hash>` prefix).
fn pod_owned_by(pod: &kindboard_core::Pod, workload: &Workload) -> bool {
    if pod.ns != workload.ns {
        return false;
    }
    match &pod.owner {
        Some(owner) => {
            owner.name == workload.name || owner.name.starts_with(&format!("{}-", workload.name))
        }
        None => false,
    }
}

/// Build the draw list: resolved nodes with status, visibility applied.
fn resolve_nodes(graph: &TopologyGraph, collapsed: &HashSet<String>) -> Vec<ViewNode> {
    let mut out = Vec::new();
    for node in &graph.layout.nodes {
        let ns = ns_of(&node.id);
        match node.kind {
            // Namespace headers are always visible; collapse hides members.
            LayoutKind::Namespace => {}
            _ if collapsed.contains(ns) => continue,
            _ => {}
        }
        let mut glyph = None;
        let status = match node.kind {
            LayoutKind::Namespace | LayoutKind::Ingress => NodeStatus::Neutral,
            LayoutKind::Workload => {
                let workload = graph
                    .workloads
                    .iter()
                    .find(|workload| workload.ns == ns && workload.name == node.name);
                match workload {
                    Some(workload) => {
                        glyph = Some(workload_glyph(workload.kind));
                        workload_status(workload, &graph.pods)
                    }
                    None => NodeStatus::Neutral,
                }
            }
            LayoutKind::Pod => {
                let pod = graph
                    .pods
                    .iter()
                    .find(|pod| pod.ns == ns && pod.name == node.name);
                match pod {
                    Some(pod) => match pod.phase {
                        PodPhase::Running => NodeStatus::Ready,
                        PodPhase::Pending => NodeStatus::Pending,
                        PodPhase::Succeeded => NodeStatus::Succeeded,
                        PodPhase::Failed => NodeStatus::Failed,
                        PodPhase::Unknown => NodeStatus::Neutral,
                    },
                    None => NodeStatus::Neutral,
                }
            }
            LayoutKind::Service => NodeStatus::Neutral,
        };
        out.push(ViewNode {
            layout: node.clone(),
            ns: ns.to_string(),
            status,
            glyph,
        });
    }
    out
}

fn workload_glyph(kind: kindboard_core::WorkloadKind) -> &'static str {
    match kind {
        kindboard_core::WorkloadKind::Deployment => "D",
        kindboard_core::WorkloadKind::StatefulSet => "SS",
        kindboard_core::WorkloadKind::DaemonSet => "DS",
        kindboard_core::WorkloadKind::Job => "J",
        kindboard_core::WorkloadKind::CronJob => "CJ",
    }
}

/// Namespace of a layout node id (`kind/ns/name` → `ns`). Namespace nodes
/// return "" so they are never filtered by their own collapsed flag.
fn ns_of(id: &str) -> &str {
    let mut parts = id.splitn(3, '/');
    match parts.next() {
        Some("namespace") => "",
        _ => parts.next().unwrap_or(""),
    }
}

/// Draw the topology of one cluster. Returns the clicked node id (if any).
// The painter is one cohesive interaction frame (pan/zoom/edges/nodes/
// legend/tooltips); splitting it into helpers would scatter the state it
// threads through without reducing its complexity.
#[allow(clippy::too_many_lines)]
pub fn show(
    ui: &mut egui::Ui,
    graph: &TopologyGraph,
    collapsed: &mut HashSet<String>,
    state: &mut DiagramState,
    selected: &Option<String>,
) -> Option<String> {
    let available = ui.available_size();
    let (rect, response) = ui.allocate_exact_size(available, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let origin = rect.min.to_vec2();

    // --- interactions ----------------------------------------------------
    if response.dragged() {
        state.offset += response.drag_delta();
    }
    let pointer = response.hover_pos();
    let zoom_delta = ui.input(|i| i.zoom_delta());
    if zoom_delta != 1.0 && pointer.is_some() {
        let anchor = pointer.unwrap_or(rect.center());
        let new_zoom = (state.zoom * zoom_delta).clamp(MIN_ZOOM, MAX_ZOOM);
        let ratio = new_zoom / state.zoom;
        // Keep the anchor point fixed under the pointer.
        state.offset = anchor.to_vec2() - (anchor.to_vec2() - state.offset - origin) * ratio;
        state.zoom = new_zoom;
    }

    // --- node list + geometry --------------------------------------------
    let nodes = resolve_nodes(graph, collapsed);
    let mut rects: Vec<(String, Rect)> = Vec::with_capacity(nodes.len());
    for node in &nodes {
        let center = origin + state.offset + egui::vec2(node.layout.x, node.layout.y) * state.zoom;
        let size = node_size(node.layout.kind) * state.zoom;
        rects.push((
            node.layout.id.clone(),
            Rect::from_center_size(center.to_pos2(), size),
        ));
    }

    if state.fit_next {
        fit_to_view(&rects, rect.size(), origin, state);
        state.fit_next = false;
    }

    // Recompute rects after a potential fit.
    let rects: Vec<(String, Rect)> = nodes
        .iter()
        .map(|node| {
            let center =
                origin + state.offset + egui::vec2(node.layout.x, node.layout.y) * state.zoom;
            let size = node_size(node.layout.kind) * state.zoom;
            (
                node.layout.id.clone(),
                Rect::from_center_size(center.to_pos2(), size),
            )
        })
        .collect();

    // --- edges (under nodes) ---------------------------------------------
    paint_edges(&painter, graph, &rects, &nodes);

    // --- nodes -----------------------------------------------------------
    for (node, node_rect) in nodes.iter().zip(rects.iter()) {
        let is_selected = selected.as_deref() == Some(node.layout.id.as_str());
        paint_node(&painter, node, node_rect.1, is_selected);
    }

    // Legend.
    paint_legend(&painter, rect, collapsed.len());

    // --- hit-testing / hover ----------------------------------------------
    let mut clicked: Option<String> = None;
    if response.clicked() {
        for (id, node_rect) in rects.iter().rev() {
            if node_rect.contains(pointer.unwrap_or_default()) {
                clicked = Some(id.clone());
                break;
            }
        }
    }
    // Hover tooltip for the node under the pointer.
    if let Some(pos) = pointer
        && response.hovered()
        && !response.dragged()
    {
        for (id, node_rect) in rects.iter().rev() {
            if node_rect.contains(pos) {
                let label = tooltip_text(graph, id);
                let galley =
                    painter.layout_no_wrap(label, egui::FontId::proportional(12.0), Color32::WHITE);
                let pad = egui::vec2(8.0, 5.0);
                let tip_rect =
                    Rect::from_min_size(pos + egui::vec2(12.0, 12.0), galley.size() + pad * 2.0);
                painter.rect(
                    tip_rect,
                    CornerRadius::same(4),
                    Color32::from_black_alpha(220),
                    Stroke::new(1.0, theme::STROKE),
                    egui::StrokeKind::Inside,
                );
                painter.galley(tip_rect.min + pad, galley, Color32::WHITE);
                break;
            }
        }
    }

    if clicked.is_none() && rects.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "No topology data yet — fetch a snapshot (Ctrl+R)",
            egui::FontId::proportional(13.0),
            theme::TEXT_DIM,
        );
    }

    clicked
}

fn node_size(kind: LayoutKind) -> Vec2 {
    match kind {
        LayoutKind::Namespace => egui::vec2(NS_W, NS_H),
        LayoutKind::Workload => egui::vec2(WK_W, WK_H),
        LayoutKind::Pod => egui::vec2(POD_W, POD_H),
        LayoutKind::Service => egui::vec2(SVC_W, SVC_H),
        LayoutKind::Ingress => egui::vec2(ING_W, ING_H),
    }
}

fn paint_node(painter: &egui::Painter, node: &ViewNode, rect: Rect, selected: bool) {
    let color = node.status.color();
    let (fill, rounding, prefix): (Color32, CornerRadius, Option<&'static str>) =
        match node.layout.kind {
            LayoutKind::Namespace => (
                Color32::from_rgba_unmultiplied(
                    theme::ACCENT.r(),
                    theme::ACCENT.g(),
                    theme::ACCENT.b(),
                    28,
                ),
                CornerRadius::same(6),
                None,
            ),
            LayoutKind::Workload => (theme::dim(color), CornerRadius::same(4), node.glyph),
            LayoutKind::Pod => (theme::dim(color), CornerRadius::same(6), None),
            LayoutKind::Service => (
                Color32::from_rgba_unmultiplied(0x5b, 0x8d, 0xb8, 40),
                CornerRadius::same(13),
                Some("svc"),
            ),
            LayoutKind::Ingress => (
                Color32::from_rgba_unmultiplied(0x8d, 0x7a, 0xb8, 40),
                CornerRadius::same(4),
                Some("ing"),
            ),
        };
    let stroke = if selected {
        Stroke::new(2.0, theme::ACCENT)
    } else {
        Stroke::new(1.0, color)
    };
    painter.rect(rect, rounding, fill, stroke, egui::StrokeKind::Inside);

    let mut label = node.layout.name.clone();
    if let Some(prefix) = prefix {
        label = format!("{prefix} {label}");
    }
    // Scale the font so long names truncate instead of overflowing.
    let max_chars = match node.layout.kind {
        LayoutKind::Namespace => 18,
        LayoutKind::Workload | LayoutKind::Pod | LayoutKind::Service | LayoutKind::Ingress => 15,
    };
    let label = truncate(&label, max_chars);
    let font_size = (11.0 * node_size(node.layout.kind).x / WK_W).max(9.0);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(font_size),
        Color32::from_rgb(0xd6, 0xe4, 0xee),
    );
}

fn tooltip_text(graph: &TopologyGraph, id: &str) -> String {
    let ns = ns_of(id);
    let name = id.rsplit('/').next().unwrap_or(id);
    let kind = id.split('/').next().unwrap_or("");
    match kind {
        "workload" => match graph
            .workloads
            .iter()
            .find(|w| w.ns == ns && w.name == name)
        {
            Some(w) => format!("{} in {} — {} labels", w.kind.as_str(), ns, w.labels.len()),
            None => name.to_string(),
        },
        "pod" => match graph.pods.iter().find(|p| p.ns == ns && p.name == name) {
            Some(p) => format!("pod {} — {:?}", p.name, p.phase),
            None => name.to_string(),
        },
        "service" => match graph.services.iter().find(|s| s.ns == ns && s.name == name) {
            Some(s) => format!("service {} — {} ports", s.name, s.ports.len()),
            None => name.to_string(),
        },
        _ => name.to_string(),
    }
}

/// Fit all visible nodes into the canvas.
fn fit_to_view(rects: &[(String, Rect)], canvas: Vec2, origin: Vec2, state: &mut DiagramState) {
    if rects.is_empty() {
        state.zoom = 1.0;
        state.offset = Vec2::ZERO;
        return;
    }
    let mut min = egui::pos2(f32::MAX, f32::MAX);
    let mut max = egui::pos2(f32::MIN, f32::MIN);
    for (_, rect) in rects {
        min.x = min.x.min(rect.min.x);
        min.y = min.y.min(rect.min.y);
        max.x = max.x.max(rect.max.x);
        max.y = max.y.max(rect.max.y);
    }
    let bbox_size = max - min;
    if bbox_size.x <= 0.0 || bbox_size.y <= 0.0 {
        state.zoom = 1.0;
        state.offset = Vec2::ZERO;
        return;
    }
    let margin = 48.0;
    let zoom = ((canvas.x - margin * 2.0) / bbox_size.x)
        .min((canvas.y - margin * 2.0) / bbox_size.y)
        .clamp(MIN_ZOOM, MAX_ZOOM);
    state.zoom = zoom;
    let content_size = bbox_size * zoom;
    let top_left = origin + (canvas - content_size) * 0.5;
    state.offset = top_left - min.to_vec2() * zoom;
}

fn paint_edges(
    painter: &egui::Painter,
    graph: &TopologyGraph,
    rects: &[(String, Rect)],
    nodes: &[ViewNode],
) {
    let id_rect: BTreeMap<&str, Rect> = rects
        .iter()
        .map(|(id, rect)| (id.as_str(), *rect))
        .collect();
    let edge_color = Color32::from_rgba_unmultiplied(0x9a, 0xa6, 0xb4, 70);

    // Service → Workload (selector match, same ns).
    for service in &graph.services {
        let from = id_rect.get(service_id(service).as_str());
        for workload in &graph.workloads {
            if workload.ns != service.ns {
                continue;
            }
            if !kindboard_core::k8s::selector_matches(&service.selector, &workload.labels) {
                continue;
            }
            if let (Some(from), Some(to)) = (
                from,
                id_rect.get(format!("workload/{}/{}", workload.ns, workload.name).as_str()),
            ) {
                line(painter, from.center(), to.center(), edge_color);
            }
        }
    }

    // Workload → Pod (direct owner).
    for workload in &graph.workloads {
        let from = id_rect.get(format!("workload/{}/{}", workload.ns, workload.name).as_str());
        for pod in &graph.pods {
            if !pod_owned_by(pod, workload) {
                continue;
            }
            if let (Some(from), Some(to)) = (
                from,
                id_rect.get(format!("pod/{}/{}", pod.ns, pod.name).as_str()),
            ) {
                line(painter, from.center(), to.center(), edge_color);
            }
        }
    }

    // Ingress → Service (backend names, same ns).
    for ingress in &graph.ingresses {
        let from = id_rect.get(format!("ingress/{}/{}", ingress.ns, ingress.name).as_str());
        for backend in &ingress.backend_services {
            if let (Some(from), Some(to)) = (
                from,
                id_rect.get(format!("service/{}/{}", ingress.ns, backend).as_str()),
            ) {
                line(painter, from.center(), to.center(), edge_color);
            }
        }
    }

    // Namespace → member guides: a faint dashed line from each namespace
    // node to its members' average x (visual grouping hint only).
    let namespace_names: Vec<&str> = nodes
        .iter()
        .filter(|node| node.layout.kind == LayoutKind::Namespace)
        .map(|node| node.layout.name.as_str())
        .collect();
    for ns in namespace_names {
        let from = id_rect.get(format!("namespace/{ns}").as_str());
        let members: Vec<Rect> = nodes
            .iter()
            .filter(|node| node.ns == ns)
            .filter_map(|node| id_rect.get(node.layout.id.as_str()).copied())
            .collect();
        if let Some(from) = from
            && !members.is_empty()
        {
            let avg_y = members.iter().map(|r| r.center().y).sum::<f32>() / members.len() as f32;
            let to = egui::pos2(from.center().x, avg_y);
            painter.line_segment(
                [from.center(), to],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(0x9a, 0xa6, 0xb4, 36)),
            );
        }
    }
}

fn service_id(service: &Service) -> String {
    format!("service/{}/{}", service.ns, service.name)
}

fn line(painter: &egui::Painter, from: Pos2, to: Pos2, color: Color32) {
    painter.line_segment([from, to], Stroke::new(1.5, color));
}

fn paint_legend(painter: &egui::Painter, canvas: Rect, collapsed_count: usize) {
    let mut x = canvas.left() + 10.0;
    let y = canvas.bottom() - 14.0;
    let entries: [(&str, Color32); 5] = [
        ("Ready/Running", theme::GREEN),
        ("Pending", theme::AMBER),
        ("Failed", theme::RED),
        ("Unknown", theme::GREY),
        ("edge: service -> workload", theme::TEXT_DIM),
    ];
    for (label, color) in entries {
        painter.circle_filled(egui::pos2(x, y), 3.0, color);
        let galley = painter.layout_no_wrap(
            label.to_string(),
            egui::FontId::proportional(10.0),
            theme::TEXT_DIM,
        );
        let galley_size = galley.size();
        painter.galley(
            egui::pos2(x + 7.0, y - galley_size.y / 2.0),
            galley,
            theme::TEXT_DIM,
        );
        x += 7.0 + galley_size.x + 14.0;
        if x > canvas.right() - 60.0 {
            break;
        }
    }
    if collapsed_count > 0 {
        let note = format!("{collapsed_count} namespace(s) collapsed");
        let galley =
            painter.layout_no_wrap(note, egui::FontId::proportional(10.0), theme::TEXT_DIM);
        painter.galley(
            egui::pos2(
                canvas.right() - galley.size().x - 10.0,
                y - galley.size().y / 2.0,
            ),
            galley,
            theme::TEXT_DIM,
        );
    }
}

// ---------------------------------------------------------------------------
// Detail side panel (clicked node).
// ---------------------------------------------------------------------------

/// Render the detail panel for the selected node id, or a hint when nothing
/// is selected.
pub fn detail_panel(ui: &mut egui::Ui, graph: &TopologyGraph, selected: &Option<String>) {
    let Some(id) = selected else {
        ui.label(
            egui::RichText::new("Select a node in the diagram to inspect it")
                .color(theme::TEXT_DIM),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Pod detail: phase, node, readiness and events. Workload detail: labels and selector.")
                .size(11.0)
                .color(theme::TEXT_DIM),
        );
        return;
    };
    let name = id.rsplit('/').next().unwrap_or(id);
    let kind = id.split('/').next().unwrap_or("");
    // Namespace nodes carry their own name as the namespace; every other
    // kind carries the member namespace in the id (`kind/ns/name`).
    let ns = if kind == "namespace" { name } else { ns_of(id) };

    match kind {
        "namespace" => {
            ui.heading(name);
            let workloads = graph.workloads.iter().filter(|w| w.ns == ns).count();
            let pods = graph.pods.iter().filter(|p| p.ns == ns).count();
            let services = graph.services.iter().filter(|s| s.ns == ns).count();
            ui.label(format!("namespace: {ns}"));
            ui.separator();
            ui.label(format!("workloads: {workloads}"));
            ui.label(format!("pods: {pods}"));
            ui.label(format!("services: {services}"));
        }
        "workload" => {
            let Some(workload) = graph
                .workloads
                .iter()
                .find(|w| w.ns == ns && w.name == name)
            else {
                ui.label(name);
                return;
            };
            ui.heading(format!("{} {}", workload.kind.as_str(), workload.name));
            ui.label(format!("namespace: {}", workload.ns));
            let pods = graph
                .pods
                .iter()
                .filter(|pod| pod_owned_by(pod, workload))
                .count();
            ui.label(format!("pods: {pods}"));
            label_map(ui, "labels", &workload.labels);
            label_map(ui, "selector", &workload.selector);
        }
        "pod" => {
            let Some(pod) = graph.pods.iter().find(|p| p.ns == ns && p.name == name) else {
                ui.label(name);
                return;
            };
            ui.heading(&pod.name);
            ui.label(format!("namespace: {}", pod.ns));
            ui.label(format!("phase: {:?}", pod.phase));
            ui.label(format!(
                "node: {}",
                pod.node.as_deref().unwrap_or("unscheduled")
            ));
            ui.label(format!(
                "ready: {}/{} containers",
                pod.ready_containers, pod.total_containers
            ));
            match &pod.owner {
                Some(owner) => {
                    ui.label(format!("owner: {} {}", owner.kind, owner.name));
                }
                None => {
                    ui.label("owner: none");
                }
            }
        }
        "service" => {
            let Some(service) = graph.services.iter().find(|s| s.ns == ns && s.name == name) else {
                ui.label(name);
                return;
            };
            ui.heading(&service.name);
            ui.label(format!("namespace: {}", service.ns));
            ui.separator();
            ui.strong("ports");
            for port in &service.ports {
                let target = port.target_port.as_deref().unwrap_or("-");
                let node_port = port
                    .node_port
                    .map_or_else(|| "-".to_string(), |value| value.to_string());
                ui.label(format!(
                    "{} {}->{} {} nodePort:{}",
                    port.name, port.port, target, port.protocol, node_port
                ));
            }
            if service.ports.is_empty() {
                ui.label("no ports");
            }
            ui.separator();
            label_map(ui, "selector", &service.selector);
        }
        "ingress" => {
            let Some(ingress) = graph
                .ingresses
                .iter()
                .find(|i| i.ns == ns && i.name == name)
            else {
                ui.label(name);
                return;
            };
            ui.heading(&ingress.name);
            ui.label(format!("namespace: {}", ingress.ns));
            ui.label(format!("class: {}", ingress.class));
            ui.label(format!("hosts: {}", ingress.hosts.join(", ")));
            ui.label(format!("backends: {}", ingress.backend_services.join(", ")));
        }
        _ => {
            ui.label(name);
        }
    }

    // Related events (ns-scoped, name or message match).
    ui.separator();
    ui.strong("events");
    let events: Vec<&K8sEvent> = graph
        .events
        .iter()
        .filter(|event| event.ns == ns && (event.name == name || event.message.contains(name)))
        .take(8)
        .collect();
    if events.is_empty() {
        ui.label(
            egui::RichText::new("no recent events for this object")
                .color(theme::TEXT_DIM)
                .size(11.0),
        );
    }
    for event in events {
        ui.label(
            egui::RichText::new(format!(
                "{} {}: {}",
                time_of(&event.timestamp),
                event.reason,
                truncate(&event.message, 90)
            ))
            .size(11.0)
            .color(theme::TEXT_DIM),
        );
    }
}

fn label_map(ui: &mut egui::Ui, title: &str, map: &BTreeMap<String, String>) {
    ui.strong(title);
    if map.is_empty() {
        ui.label(
            egui::RichText::new("(none)")
                .color(theme::TEXT_DIM)
                .size(11.0),
        );
        return;
    }
    for (key, value) in map {
        ui.label(
            egui::RichText::new(format!("{key}: {value}"))
                .size(11.0)
                .color(theme::TEXT_DIM),
        );
    }
}

/// Namespace collapse state for the diagram.
pub type CollapsedNamespaces = HashSet<String>;

#[cfg(test)]
mod tests {
    use super::*;

    fn pod(name: &str, ns: &str, owner: Option<(&str, &str)>) -> kindboard_core::Pod {
        kindboard_core::Pod {
            name: name.to_string(),
            ns: ns.to_string(),
            owner: owner.map(|(kind, name)| kindboard_core::OwnerRef {
                kind: kind.to_string(),
                name: name.to_string(),
            }),
            phase: PodPhase::Running,
            node: None,
            ready_containers: 1,
            total_containers: 1,
        }
    }

    fn workload(name: &str, ns: &str) -> Workload {
        Workload {
            kind: kindboard_core::WorkloadKind::Deployment,
            name: name.to_string(),
            ns: ns.to_string(),
            selector: Default::default(),
            labels: Default::default(),
        }
    }

    #[test]
    fn pod_owned_by_exact_owner_name() {
        let pod = pod("api-0", "default", Some(("StatefulSet", "api")));
        assert!(pod_owned_by(&pod, &workload("api", "default")));
    }

    #[test]
    fn pod_owned_by_replicaset_prefix() {
        let pod = pod("web-abc12", "default", Some(("ReplicaSet", "web-84f9c")));
        assert!(pod_owned_by(&pod, &workload("web", "default")));
    }

    #[test]
    fn pod_not_owned_across_namespaces() {
        let pod = pod("web-0", "kube-system", Some(("ReplicaSet", "web-1")));
        assert!(!pod_owned_by(&pod, &workload("web", "default")));
    }

    #[test]
    fn pod_without_owner_is_unowned() {
        let pod = pod("lone", "default", None);
        assert!(!pod_owned_by(&pod, &workload("lone", "default")));
    }

    #[test]
    fn fit_to_view_sets_sane_zoom() {
        let rects = vec![
            (
                "a".to_string(),
                Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 50.0)),
            ),
            (
                "b".to_string(),
                Rect::from_min_size(egui::pos2(200.0, 140.0), egui::vec2(100.0, 50.0)),
            ),
        ];
        let mut state = DiagramState::new();
        fit_to_view(&rects, egui::vec2(600.0, 400.0), Vec2::ZERO, &mut state);
        assert!(state.zoom > MIN_ZOOM);
        assert!(state.zoom <= MAX_ZOOM);
    }

    #[test]
    fn ns_of_parses_layout_ids() {
        assert_eq!(ns_of("workload/default/web"), "default");
        assert_eq!(ns_of("namespace/kube-system"), "");
        assert_eq!(ns_of("service/a/b/c"), "a");
    }
}
