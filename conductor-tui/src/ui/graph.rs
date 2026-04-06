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

/// Minimum node width — enforced even on tiny terminals.
const NODE_WIDTH_MIN: u16 = 24;
/// Maximum node width — prevents absurdly wide nodes on large terminals.
const NODE_WIDTH_MAX: u16 = 72;
pub const NODE_HEIGHT: u16 = 4;
const H_GAP: u16 = 4; // horizontal gap between layers
const V_GAP: u16 = 1; // vertical gap between nodes in a layer

// ─── GraphNode trait ────────────────────────────────────────────────────────

pub trait GraphNode {
    fn id(&self) -> &str;
    /// Short identifier line (e.g. "#1234")
    fn label(&self) -> String;
    /// Optional second content line (full title — truncation handled at render time)
    fn title_line(&self) -> Option<String> {
        None
    }
    fn subtitle(&self) -> Option<String>;
    fn status_style(&self) -> Style;
    /// Whether this node has an active worktree (shown as ● indicator)
    fn has_active_worktree(&self) -> bool {
        false
    }
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

impl<N> GraphData<N> {
    /// Topological sort into layers for DAG layout.
    /// Returns a Vec of layers, each layer holding the node IDs (strings) at that depth.
    /// Nodes with no edges are excluded (they are tracked via `unconnected_count`).
    pub fn compute_layers(&self) -> Vec<Vec<String>> {
        if self.nodes.is_empty() {
            return Vec::new();
        }

        let mut connected_ids: HashSet<&str> = HashSet::new();
        for edge in &self.edges {
            connected_ids.insert(&edge.from);
            connected_ids.insert(&edge.to);
        }

        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut in_degree: HashMap<&str, usize> = HashMap::new();

        for id in &connected_ids {
            adj.entry(id).or_default();
            in_degree.entry(id).or_insert(0);
        }

        for edge in &self.edges {
            if connected_ids.contains(edge.from.as_str())
                && connected_ids.contains(edge.to.as_str())
            {
                adj.entry(&edge.from).or_default().push(&edge.to);
                *in_degree.entry(&edge.to).or_insert(0) += 1;
            }
        }

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

        for id in &connected_ids {
            layer_of.entry(id).or_insert(0);
        }

        let max_layer = layer_of.values().copied().max().unwrap_or(0);
        let mut layers: Vec<Vec<&str>> = vec![Vec::new(); max_layer + 1];
        for &id in &connected_ids {
            layers[layer_of[id]].push(id);
        }

        // Build reverse adjacency map once for O(1) predecessor lookup during barycenter ordering.
        let mut pred_adj: HashMap<&str, Vec<&str>> = HashMap::new();
        for (&src, children) in &adj {
            for &child in children {
                pred_adj.entry(child).or_default().push(src);
            }
        }

        // Barycenter ordering
        let mut layer_pos: HashMap<&str, usize> = HashMap::new();
        for (l, layer_nodes) in layers.iter_mut().enumerate() {
            if l == 0 {
                layer_nodes.sort();
            } else {
                // Build a HashMap of barycenters so the sort comparator is O(1).
                let barycenters: HashMap<&str, f64> = layer_nodes
                    .iter()
                    .map(|&node| {
                        let preds: Vec<f64> = pred_adj
                            .get(node)
                            .map(|ps| ps.as_slice())
                            .unwrap_or(&[])
                            .iter()
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
                    let ba = barycenters.get(a).copied().unwrap_or(0.0);
                    let bb = barycenters.get(b).copied().unwrap_or(0.0);
                    ba.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            for (pos, &node) in layer_nodes.iter().enumerate() {
                layer_pos.insert(node, pos);
            }
        }

        layers
            .into_iter()
            .map(|l| l.into_iter().map(|s| s.to_string()).collect())
            .collect()
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
    pub has_worktree: bool,
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
            has_worktree: false,
        }
    }
}

impl GraphNode for TicketGraphNode {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> String {
        format!("#{}", self.source_id)
    }

    fn title_line(&self) -> Option<String> {
        Some(self.title.clone())
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

    fn has_active_worktree(&self) -> bool {
        self.has_worktree
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
        self.step_name.clone()
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

    fn title_line(&self) -> Option<String> {
        match self {
            GraphNodeType::Ticket(n) => n.title_line(),
            GraphNodeType::WorkflowStep(n) => n.title_line(),
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

    fn has_active_worktree(&self) -> bool {
        match self {
            GraphNodeType::Ticket(n) => n.has_active_worktree(),
            GraphNodeType::WorkflowStep(_) => false,
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
/// `layers` must be the pre-computed result of `data.compute_layers()`.
/// `node_width` and `h_gap` control horizontal spacing and are computed
/// dynamically by the caller based on available screen width.
pub fn compute_layout(layers: Vec<Vec<String>>, node_width: u16, h_gap: u16) -> ComputedLayout {
    if layers.is_empty() {
        return ComputedLayout::default();
    }

    let mut node_positions: HashMap<String, (u16, u16)> = HashMap::new();
    for (layer_idx, layer_nodes) in layers.iter().enumerate() {
        let x = layer_idx as u16 * (node_width + h_gap);
        for (node_idx, node_id) in layer_nodes.iter().enumerate() {
            let y = node_idx as u16 * (NODE_HEIGHT + V_GAP);
            node_positions.insert(node_id.clone(), (x, y));
        }
    }

    ComputedLayout {
        layers,
        node_positions,
    }
}

// ─── Rendering ───────────────────────────────────────────────────────────────

/// Truncate a string to `max_chars`, appending `…` if truncated.
/// Operates on char boundaries to avoid panics on multibyte chars.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{}…", truncated)
    }
}

/// Render the graph view content area (edges via Canvas, nodes via Block/Paragraph).
/// `node_width` is the dynamic width computed by the caller.
pub fn render_graph(
    frame: &mut Frame,
    area: Rect,
    data: &GraphData<GraphNodeType>,
    nav: &GraphNavState,
    layout: &ComputedLayout,
    node_width: u16,
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

    // Helper: apply pan offset, return screen Rect (clamped to area), or None if off-screen.
    let node_rect = |base_x: u16, base_y: u16| -> Option<Rect> {
        let x = base_x as i32 - pan_x as i32;
        let y = base_y as i32 - pan_y as i32;
        if x < 0
            || y < 0
            || x as u16 + node_width > area.width
            || y as u16 + NODE_HEIGHT > area.height
        {
            let rx = (area.x + x.max(0) as u16).min(area.x + area.width.saturating_sub(1));
            let ry = (area.y + y.max(0) as u16).min(area.y + area.height.saturating_sub(1));
            if x as u16 >= area.width || y as u16 >= area.height {
                return None;
            }
            let w = node_width.min(area.width.saturating_sub(rx - area.x));
            let h = NODE_HEIGHT.min(area.height.saturating_sub(ry - area.y));
            if w == 0 || h == 0 {
                return None;
            }
            Some(Rect::new(rx, ry, w, h))
        } else {
            Some(Rect::new(
                area.x + x as u16,
                area.y + y as u16,
                node_width,
                NODE_HEIGHT,
            ))
        }
    };

    let node_map: HashMap<&str, &GraphNodeType> = data.nodes.iter().map(|n| (n.id(), n)).collect();

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
    let node_width_f = node_width as f64;

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

                let src_x = sx as f64 + node_width_f - pan_x_f;
                let src_y_screen = sy as f64 + NODE_HEIGHT as f64 / 2.0 - pan_y_f;
                let tgt_x = tx as f64 - pan_x_f;
                let tgt_y_screen = ty as f64 + NODE_HEIGHT as f64 / 2.0 - pan_y_f;

                // Canvas y is inverted (0 = bottom, h = top)
                let src_y = h - src_y_screen;
                let tgt_y = h - tgt_y_screen;
                let mid_x = (src_x + tgt_x) / 2.0;

                if edge.edge_type == EdgeType::BlockedBy {
                    draw_dashed_hline(ctx, src_x, mid_x, src_y, color);
                    draw_dashed_vline(ctx, mid_x, src_y.min(tgt_y), src_y.max(tgt_y), color);
                    draw_dashed_hline(ctx, mid_x, tgt_x, tgt_y, color);
                } else {
                    ctx.draw(&CanvasLine::new(src_x, src_y, mid_x, src_y, color));
                    ctx.draw(&CanvasLine::new(mid_x, src_y, mid_x, tgt_y, color));
                    ctx.draw(&CanvasLine::new(mid_x, tgt_y, tgt_x, tgt_y, color));
                }
            }
        });

    frame.render_widget(canvas, area);

    // ── Pass 2: Draw node boxes ──────────────────────────────────────────────
    // Inner width available for text: node_width - 2 borders - 1 leading space
    let text_width = (node_width as usize).saturating_sub(3);

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
            let label_style = if is_selected {
                Style::default()
                    .fg(theme.label_accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.label_accent)
            };
            let (wt_dot, wt_dot_style) = if node.has_active_worktree() {
                ("● ", Style::default().fg(theme.status_completed))
            } else {
                ("○ ", Style::default().fg(theme.label_secondary))
            };

            // Title line: truncate to fit the actual node width at render time.
            // "● " dot takes 2 chars on the label line, title gets the full inner width.
            let title_line = if let Some(t) = node.title_line() {
                let display = truncate_str(&t, text_width);
                Line::from(Span::styled(
                    format!(" {}", display),
                    if is_selected {
                        Style::default()
                            .fg(theme.label_primary)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.label_primary)
                    },
                ))
            } else {
                Line::from("")
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
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(wt_dot, wt_dot_style),
                    Span::styled(label, label_style),
                ]),
                title_line,
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

    // Split: title bar (2) + content (fill) + footer (2)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(area);

    let title_area = chunks[0];
    let content_area = chunks[1];
    let footer_area = chunks[2];

    // ── Dynamic node sizing ──────────────────────────────────────────────────
    // Compute layers once (topology-only pass) to determine count for node width,
    // then pass the pre-computed layers directly into compute_layout.
    let layers = data.compute_layers();
    let num_layers = layers.len().max(1);
    let node_width = if content_area.width == 0 {
        NODE_WIDTH_MIN
    } else {
        let total_gaps = (num_layers as u16).saturating_sub(1) * H_GAP;
        let available = content_area.width.saturating_sub(total_gaps);
        (available / num_layers as u16).clamp(NODE_WIDTH_MIN, NODE_WIDTH_MAX)
    };

    let layout = compute_layout(layers, node_width, H_GAP);

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
    render_graph(frame, content_area, data, nav, &layout, node_width, theme);
}
