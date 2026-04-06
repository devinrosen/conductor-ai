use std::collections::{HashMap, HashSet, VecDeque};

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use conductor_core::tickets::Ticket;

use crate::theme::Theme;

// ─── Node dimensions (characters) ──────────────────────────────────────────

pub const NODE_WIDTH: u16 = 22;
pub const NODE_HEIGHT: u16 = 3;
const H_GAP: u16 = 8; // horizontal gap between layers
const V_GAP: u16 = 1; // vertical gap between nodes in a layer

// ─── GraphNode trait ────────────────────────────────────────────────────────

pub trait GraphNode {
    fn id(&self) -> &str;
    fn label(&self) -> String;
    fn subtitle(&self) -> Option<String>;
    fn status_style(&self) -> Style;
}

// ─── Edge types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum EdgeType {
    /// This node is blocked by the source node
    BlockedBy,
    /// Parent → child hierarchy
    ParentChild,
    /// Sequential workflow step
    Sequential,
    /// Parallel workflow step
    Parallel,
}

#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub edge_type: EdgeType,
}

// ─── Graph data container ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GraphData<N> {
    pub nodes: Vec<N>,
    pub edges: Vec<GraphEdge>,
    /// Count of nodes that have no edges (excluded from the DAG layout).
    pub unconnected_count: usize,
}

impl<N> Default for GraphData<N> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            unconnected_count: 0,
        }
    }
}

// ─── Nav state ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct GraphNavState {
    pub selected_layer: usize,
    pub selected_node_idx: usize,
    pub pan_x: i16,
    pub pan_y: i16,
}

// ─── Concrete node types ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TicketGraphNode {
    pub id: String,
    pub source_id: String,
    pub title: String,
    pub state: String,
    #[allow(dead_code)]
    pub labels: String,
    #[allow(dead_code)]
    pub assignee: Option<String>,
}

impl TicketGraphNode {
    pub fn from_ticket(t: &Ticket) -> Self {
        Self {
            id: t.id.clone(),
            source_id: t.source_id.clone(),
            title: t.title.clone(),
            state: t.state.clone(),
            labels: t.labels.clone(),
            assignee: t.assignee.clone(),
        }
    }
}

impl GraphNode for TicketGraphNode {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> String {
        let truncated = if self.title.len() > 16 {
            format!("{}…", &self.title[..15])
        } else {
            self.title.clone()
        };
        format!("#{} {}", self.source_id, truncated)
    }

    fn subtitle(&self) -> Option<String> {
        Some(self.state.clone())
    }

    fn status_style(&self) -> Style {
        match self.state.as_str() {
            "open" => Style::default().fg(Color::Green),
            "closed" => Style::default().fg(Color::DarkGray),
            _ => Style::default().fg(Color::Yellow),
        }
    }
}

/// Stub node type for future workflow step graph support.
#[derive(Debug, Clone)]
pub struct WorkflowStepGraphNode {
    pub id: String,
    pub step_name: String,
    pub status: String,
    pub duration: Option<String>,
}

impl GraphNode for WorkflowStepGraphNode {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> String {
        if self.step_name.len() > 18 {
            format!("{}…", &self.step_name[..17])
        } else {
            self.step_name.clone()
        }
    }

    fn subtitle(&self) -> Option<String> {
        self.duration
            .as_ref()
            .map(|d| format!("{} ({})", self.status, d))
            .or_else(|| Some(self.status.clone()))
    }

    fn status_style(&self) -> Style {
        match self.status.as_str() {
            "completed" | "success" => Style::default().fg(Color::Green),
            "failed" | "error" => Style::default().fg(Color::Red),
            "running" | "active" => Style::default().fg(Color::Yellow),
            "pending" | "waiting" => Style::default().fg(Color::Cyan),
            _ => Style::default().fg(Color::White),
        }
    }
}

/// Enum wrapper so Modal can hold heterogeneous node types without generics.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum GraphNodeType {
    Ticket(TicketGraphNode),
    WorkflowStep(WorkflowStepGraphNode),
}

impl GraphNode for GraphNodeType {
    fn id(&self) -> &str {
        match self {
            GraphNodeType::Ticket(n) => n.id(),
            GraphNodeType::WorkflowStep(n) => n.id(),
        }
    }

    fn label(&self) -> String {
        match self {
            GraphNodeType::Ticket(n) => n.label(),
            GraphNodeType::WorkflowStep(n) => n.label(),
        }
    }

    fn subtitle(&self) -> Option<String> {
        match self {
            GraphNodeType::Ticket(n) => n.subtitle(),
            GraphNodeType::WorkflowStep(n) => n.subtitle(),
        }
    }

    fn status_style(&self) -> Style {
        match self {
            GraphNodeType::Ticket(n) => n.status_style(),
            GraphNodeType::WorkflowStep(n) => n.status_style(),
        }
    }
}

// ─── Computed layout ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ComputedLayout {
    /// layers[i] = ordered list of node IDs in layer i
    pub layers: Vec<Vec<String>>,
    /// node_id -> (screen_x, screen_y) — top-left corner of node box
    pub node_positions: HashMap<String, (u16, u16)>,
}

// ─── Layout algorithm ────────────────────────────────────────────────────────

/// Compute a layered DAG layout for the connected nodes in `data`.
/// Unconnected nodes are counted in `data.unconnected_count` and excluded.
pub fn compute_layout(data: &GraphData<GraphNodeType>) -> ComputedLayout {
    if data.nodes.is_empty() {
        return ComputedLayout::default();
    }

    // Collect IDs of connected nodes (nodes with at least one edge)
    let mut connected_ids: HashSet<&str> = HashSet::new();
    for edge in &data.edges {
        connected_ids.insert(&edge.from);
        connected_ids.insert(&edge.to);
    }

    // Build adjacency (from -> to) and in-degree for Kahn's algorithm
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut in_degree: HashMap<&str, usize> = HashMap::new();

    for id in &connected_ids {
        adj.entry(id).or_default();
        in_degree.entry(id).or_insert(0);
    }

    for edge in &data.edges {
        // Only include edges between connected nodes
        if connected_ids.contains(edge.from.as_str()) && connected_ids.contains(edge.to.as_str()) {
            adj.entry(&edge.from).or_default().push(&edge.to);
            *in_degree.entry(&edge.to).or_insert(0) += 1;
        }
    }

    // Kahn's topological sort → assign longest-path layer
    let mut layer_of: HashMap<&str, usize> = HashMap::new();

    let mut queue: VecDeque<&str> = VecDeque::new();
    for (&id, &deg) in &in_degree {
        if deg == 0 {
            queue.push_back(id);
            layer_of.insert(id, 0);
        }
    }

    while let Some(node) = queue.pop_front() {
        let cur_layer = layer_of[node];
        if let Some(children) = adj.get(node) {
            for &child in children {
                let new_layer = cur_layer + 1;
                let entry = layer_of.entry(child).or_insert(0);
                if new_layer > *entry {
                    *entry = new_layer;
                }
                let deg = in_degree.get_mut(child).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(child);
                }
            }
        }
    }

    // Nodes not reachable by Kahn's (cycles) go to layer 0
    for id in &connected_ids {
        layer_of.entry(id).or_insert(0);
    }

    // Group nodes by layer
    let max_layer = layer_of.values().copied().max().unwrap_or(0);
    let mut layers: Vec<Vec<&str>> = vec![Vec::new(); max_layer + 1];
    for &id in &connected_ids {
        let l = layer_of[id];
        layers[l].push(id);
    }

    // Barycenter ordering within each layer (single forward pass)
    // For layer > 0: sort by avg layer-position of predecessors
    let mut layer_pos: HashMap<&str, usize> = HashMap::new();
    for (l, layer_nodes) in layers.iter_mut().enumerate() {
        if l == 0 {
            // Sort layer 0 by original order for stability
            layer_nodes.sort();
        } else {
            // Compute barycenter for each node
            let barycenters: Vec<(&str, f64)> = layer_nodes
                .iter()
                .map(|&node| {
                    // Find predecessors (nodes in adj that point to this node)
                    let preds: Vec<f64> = connected_ids
                        .iter()
                        .filter(|&&src| {
                            adj.get(src)
                                .map(|children| children.contains(&node))
                                .unwrap_or(false)
                        })
                        .filter_map(|&src| layer_pos.get(src).map(|&p| p as f64))
                        .collect();
                    let bc = if preds.is_empty() {
                        0.0
                    } else {
                        preds.iter().sum::<f64>() / preds.len() as f64
                    };
                    (node, bc)
                })
                .collect();
            layer_nodes.sort_by(|a, b| {
                let ba = barycenters
                    .iter()
                    .find(|(n, _)| n == a)
                    .map(|(_, bc)| *bc)
                    .unwrap_or(0.0);
                let bb = barycenters
                    .iter()
                    .find(|(n, _)| n == b)
                    .map(|(_, bc)| *bc)
                    .unwrap_or(0.0);
                ba.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        for (pos, &node) in layer_nodes.iter().enumerate() {
            layer_pos.insert(node, pos);
        }
    }

    // Assign screen coordinates
    let mut node_positions: HashMap<String, (u16, u16)> = HashMap::new();
    for (layer_idx, layer_nodes) in layers.iter().enumerate() {
        let x = layer_idx as u16 * (NODE_WIDTH + H_GAP);
        for (node_idx, &node_id) in layer_nodes.iter().enumerate() {
            let y = node_idx as u16 * (NODE_HEIGHT + V_GAP);
            node_positions.insert(node_id.to_string(), (x, y));
        }
    }

    ComputedLayout {
        layers: layers
            .into_iter()
            .map(|l| l.into_iter().map(|s| s.to_string()).collect())
            .collect(),
        node_positions,
    }
}

// ─── Rendering ───────────────────────────────────────────────────────────────

/// Render the graph view content area (edges via Canvas, nodes via Block/Paragraph).
pub fn render_graph(
    frame: &mut Frame,
    area: Rect,
    data: &GraphData<GraphNodeType>,
    nav: &GraphNavState,
    layout: &ComputedLayout,
    theme: &Theme,
) {
    if layout.node_positions.is_empty() {
        let msg = Paragraph::new(Line::from(Span::styled(
            "No dependency edges found — all tickets are independent.",
            Style::default().fg(theme.label_secondary),
        )));
        frame.render_widget(msg, area);
        return;
    }

    let pan_x = nav.pan_x;
    let pan_y = nav.pan_y;

    // Helper: apply pan offset and check visibility
    let node_rect = |base_x: u16, base_y: u16| -> Option<Rect> {
        let x = base_x as i32 - pan_x as i32;
        let y = base_y as i32 - pan_y as i32;
        if x < 0
            || y < 0
            || x as u16 + NODE_WIDTH > area.width
            || y as u16 + NODE_HEIGHT > area.height
        {
            // Allow partial visibility: clamp to area
            let rx = (area.x + x.max(0) as u16).min(area.x + area.width.saturating_sub(1));
            let ry = (area.y + y.max(0) as u16).min(area.y + area.height.saturating_sub(1));
            if x as u16 >= area.width || y as u16 >= area.height {
                return None;
            }
            let w = NODE_WIDTH.min(area.width.saturating_sub(rx - area.x));
            let h = NODE_HEIGHT.min(area.height.saturating_sub(ry - area.y));
            if w == 0 || h == 0 {
                return None;
            }
            Some(Rect::new(rx, ry, w, h))
        } else {
            Some(Rect::new(
                area.x + x as u16,
                area.y + y as u16,
                NODE_WIDTH,
                NODE_HEIGHT,
            ))
        }
    };

    // Build node lookup by id
    let node_map: HashMap<&str, &GraphNodeType> = data.nodes.iter().map(|n| (n.id(), n)).collect();

    // Identify selected node id
    let selected_id = layout
        .layers
        .get(nav.selected_layer)
        .and_then(|l| l.get(nav.selected_node_idx))
        .map(|s| s.as_str());

    // ── Pass 1: Draw edges via Canvas ────────────────────────────────────────
    let w = area.width as f64;
    let h = area.height as f64;

    let edges = data.edges.clone();
    let positions = layout.node_positions.clone();
    let pan_x_f = pan_x as f64;
    let pan_y_f = pan_y as f64;

    let canvas = Canvas::default()
        .x_bounds([0.0, w])
        .y_bounds([0.0, h])
        .paint(move |ctx| {
            for edge in &edges {
                let Some(&(sx, sy)) = positions.get(&edge.from) else {
                    continue;
                };
                let Some(&(tx, ty)) = positions.get(&edge.to) else {
                    continue;
                };

                let color = match edge.edge_type {
                    EdgeType::BlockedBy => Color::Yellow,
                    EdgeType::ParentChild => Color::DarkGray,
                    EdgeType::Sequential => Color::Cyan,
                    EdgeType::Parallel => Color::Blue,
                };

                // Source right-center (screen coords)
                let src_x = sx as f64 + NODE_WIDTH as f64 - pan_x_f;
                let src_y_screen = sy as f64 + NODE_HEIGHT as f64 / 2.0 - pan_y_f;

                // Target left-center
                let tgt_x = tx as f64 - pan_x_f;
                let tgt_y_screen = ty as f64 + NODE_HEIGHT as f64 / 2.0 - pan_y_f;

                // Canvas y is inverted (0 = bottom, h = top)
                let src_y = h - src_y_screen;
                let tgt_y = h - tgt_y_screen;

                let mid_x = (src_x + tgt_x) / 2.0;

                if edge.edge_type == EdgeType::BlockedBy {
                    // Dashed: draw short segments with gaps
                    draw_dashed_hline(ctx, src_x, mid_x, src_y, color);
                    draw_dashed_vline(ctx, mid_x, src_y.min(tgt_y), src_y.max(tgt_y), color);
                    draw_dashed_hline(ctx, mid_x, tgt_x, tgt_y, color);
                } else {
                    // Solid L-shaped path
                    ctx.draw(&CanvasLine::new(src_x, src_y, mid_x, src_y, color));
                    ctx.draw(&CanvasLine::new(mid_x, src_y, mid_x, tgt_y, color));
                    ctx.draw(&CanvasLine::new(mid_x, tgt_y, tgt_x, tgt_y, color));
                }
            }
        });

    frame.render_widget(canvas, area);

    // ── Pass 2: Draw node boxes ──────────────────────────────────────────────
    for (layer_idx, layer_nodes) in layout.layers.iter().enumerate() {
        for (node_idx, node_id) in layer_nodes.iter().enumerate() {
            let Some(&(base_x, base_y)) = layout.node_positions.get(node_id) else {
                continue;
            };
            let Some(rect) = node_rect(base_x, base_y) else {
                continue;
            };
            let Some(node) = node_map.get(node_id.as_str()) else {
                continue;
            };

            let is_selected = Some(node_id.as_str()) == selected_id
                && layer_idx == nav.selected_layer
                && node_idx == nav.selected_node_idx;

            let border_style = if is_selected {
                Style::default()
                    .fg(theme.border_focused)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.border_inactive)
            };

            let label = node.label();
            let label_truncated = if label.len() > (NODE_WIDTH as usize).saturating_sub(2) {
                format!("{}…", &label[..(NODE_WIDTH as usize).saturating_sub(3)])
            } else {
                label
            };

            let subtitle_line = if let Some(sub) = node.subtitle() {
                let style = node.status_style();
                Line::from(Span::styled(
                    format!(" {}", sub),
                    style.add_modifier(Modifier::DIM),
                ))
            } else {
                Line::from("")
            };

            let content = Paragraph::new(vec![
                Line::from(Span::styled(
                    format!(" {}", label_truncated),
                    if is_selected {
                        Style::default()
                            .fg(theme.label_primary)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.label_primary)
                    },
                )),
                subtitle_line,
            ])
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style),
            );

            frame.render_widget(Clear, rect);
            frame.render_widget(content, rect);
        }
    }
}

// ─── Dashed line helpers ─────────────────────────────────────────────────────

fn draw_dashed_hline(
    ctx: &mut ratatui::widgets::canvas::Context,
    x1: f64,
    x2: f64,
    y: f64,
    color: Color,
) {
    let (start, end) = if x1 <= x2 { (x1, x2) } else { (x2, x1) };
    let mut x = start;
    let dash = 2.0;
    let gap = 1.0;
    while x < end {
        let seg_end = (x + dash).min(end);
        ctx.draw(&CanvasLine::new(x, y, seg_end, y, color));
        x += dash + gap;
    }
}

fn draw_dashed_vline(
    ctx: &mut ratatui::widgets::canvas::Context,
    x: f64,
    y1: f64,
    y2: f64,
    color: Color,
) {
    let (start, end) = if y1 <= y2 { (y1, y2) } else { (y2, y1) };
    let mut y = start;
    let dash = 2.0;
    let gap = 1.0;
    while y < end {
        let seg_end = (y + dash).min(end);
        ctx.draw(&CanvasLine::new(x, y, x, seg_end, color));
        y += dash + gap;
    }
}

// ─── Graph view wrapper (called from modal.rs) ───────────────────────────────

pub fn render_graph_view(
    frame: &mut Frame,
    area: Rect,
    data: &GraphData<GraphNodeType>,
    nav: &GraphNavState,
    title: &str,
    theme: &Theme,
) {
    use ratatui::layout::{Constraint, Direction, Layout};

    // Compute layout (done every render; cheap for typical graph sizes)
    let layout = compute_layout(data);

    // Split: title bar (1) + content (fill) + footer (1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    let title_area = chunks[0];
    let content_area = chunks[1];
    let footer_area = chunks[2];

    // Title bar
    let title_line = Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled(
            title,
            Style::default()
                .fg(theme.label_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  [Esc] close  [h/j/k/l] navigate  [H/J/K/L] pan  [Enter] detail",
            Style::default().fg(theme.label_secondary),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(title_line).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(theme.border_inactive)),
        ),
        title_area,
    );

    // Footer
    let unconnected_msg = if data.unconnected_count > 0 {
        format!(
            "  {} ticket(s) with no dependencies not shown",
            data.unconnected_count
        )
    } else {
        String::new()
    };

    // Show selected node info in footer
    let selected_label = layout
        .layers
        .get(nav.selected_layer)
        .and_then(|l| l.get(nav.selected_node_idx))
        .and_then(|id| data.nodes.iter().find(|n| n.id() == id.as_str()))
        .map(|n| n.label())
        .unwrap_or_default();

    let footer_line = Line::from(vec![
        Span::styled(
            format!(
                "  Layer {}/{}  Node {}/{}",
                nav.selected_layer + 1,
                layout.layers.len().max(1),
                nav.selected_node_idx + 1,
                layout
                    .layers
                    .get(nav.selected_layer)
                    .map(|l| l.len())
                    .unwrap_or(0)
                    .max(1),
            ),
            Style::default().fg(theme.label_accent),
        ),
        Span::styled(
            if selected_label.is_empty() {
                String::new()
            } else {
                format!("  {}", selected_label)
            },
            Style::default().fg(theme.label_primary),
        ),
        Span::styled(unconnected_msg, Style::default().fg(theme.label_secondary)),
    ]);
    frame.render_widget(
        Paragraph::new(footer_line).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(theme.border_inactive)),
        ),
        footer_area,
    );

    // Graph content
    render_graph(frame, content_area, data, nav, &layout, theme);
}
