use std::time::{Duration, Instant};

use crate::api::schema::{GitCommit, GitHistoryParams, Method, ResponseResult};

use super::render::put_text;
use super::*;
use crate::ui::truncate_end;

const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GraphTarget {
    endpoint: ClientEndpointId,
    boot_id: String,
    workspace_id: String,
    cwd: String,
}

pub(super) struct GraphRow {
    pub commit: GitCommit,
    node: String,
    edges: String,
}

#[derive(Default)]
pub(super) struct GitGraphState {
    target: Option<GraphTarget>,
    pub rows: Vec<GraphRow>,
    pub scroll: usize,
    head: Option<String>,
    has_more: bool,
    pending: bool,
    next_refresh: Option<Instant>,
    pub message: Option<String>,
}

impl ClientShellState {
    pub(super) fn sync_git_graph_target(&mut self) -> bool {
        let target = self.snapshot.as_deref().and_then(|snapshot| {
            let pane = snapshot
                .panes
                .iter()
                .find(|pane| Some(pane.pane_id.as_str()) == snapshot.focused_pane_id.as_deref())?;
            Some(GraphTarget {
                endpoint: self.active_endpoint_id.clone(),
                boot_id: snapshot.boot_id.clone(),
                workspace_id: pane.workspace_id.clone(),
                cwd: pane.foreground_cwd.as_ref().or(pane.cwd.as_ref())?.clone(),
            })
        });
        if self.git_graph.target == target {
            return false;
        }
        self.git_graph = GitGraphState {
            target,
            ..Default::default()
        };
        self.hits.graph_copy.clear();
        true
    }

    pub(crate) fn tick_git_graph(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        outcome.repaint |= self.sync_git_graph_target();
        if self.sidebar_collapsed || self.hits.graph_body.is_empty() || self.git_graph.pending {
            return;
        }
        let Some(target) = self.git_graph.target.clone() else {
            return;
        };
        let available = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == target.endpoint)
            .is_some_and(|endpoint| {
                endpoint.status == ClientEndpointStatus::Online
                    && endpoint
                        .methods
                        .as_ref()
                        .is_some_and(|methods| methods.contains("git.history"))
            });
        if !available {
            let message = "History unavailable on this server";
            if self.git_graph.message.as_deref() != Some(message) {
                self.git_graph.message = Some(message.into());
                outcome.repaint = true;
            }
            return;
        }
        let more = self.git_graph.has_more
            && self.git_graph.scroll > 0
            && self
                .git_graph
                .scroll
                .saturating_add(usize::from(self.hits.graph_body.height / 2) + 5)
                >= self.git_graph.rows.len();
        if !self.pending_requests.is_empty()
            || self.git_graph.message.is_some()
                && self.git_graph.next_refresh.is_some_and(|due| now < due)
            || !more
                && (self.git_graph.scroll > 0 && self.git_graph.message.is_none()
                    || self.git_graph.next_refresh.is_some_and(|due| now < due))
        {
            return;
        }
        let skip = if more { self.git_graph.rows.len() } else { 0 };
        let method = Method::GitHistory(GitHistoryParams {
            cwd: target.cwd.clone(),
            revision: more.then(|| self.git_graph.head.clone()).flatten(),
            known_head: (!more).then(|| self.git_graph.head.clone()).flatten(),
            skip,
            limit: 100,
        });
        self.git_graph.pending = self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::GitHistory { target, skip },
            outcome,
        );
        self.git_graph.next_refresh = Some(now + REFRESH_INTERVAL);
    }

    pub(super) fn complete_git_history(
        &mut self,
        target: GraphTarget,
        skip: usize,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        self.sync_git_graph_target();
        if self.git_graph.target.as_ref() != Some(&target) {
            return (false, Vec::new());
        }
        let graph = &mut self.git_graph;
        graph.pending = false;
        graph.next_refresh = Some(Instant::now() + REFRESH_INTERVAL);
        match result {
            Ok(ResponseResult::GitHistory { history }) => {
                let had_message = graph.message.take().is_some();
                if history.unchanged {
                    return (had_message, Vec::new());
                }
                if skip == 0 {
                    graph.rows.clear();
                    graph.scroll = 0;
                } else if graph.head != history.head || graph.rows.len() != skip {
                    return (false, Vec::new());
                }
                graph.head = history.head;
                graph.has_more = history.has_more;
                graph
                    .rows
                    .extend(history.commits.into_iter().map(|commit| GraphRow {
                        commit,
                        node: String::new(),
                        edges: String::new(),
                    }));
                build_lanes(&mut graph.rows);
            }
            Err(error) => graph.message = Some(error.message),
            Ok(_) => graph.message = Some("Unexpected Git history response".into()),
        }
        (true, Vec::new())
    }
}

fn build_lanes(rows: &mut [GraphRow]) {
    let mut lanes: Vec<String> = Vec::new();
    for row in rows {
        let column = lanes
            .iter()
            .position(|id| id == &row.commit.id)
            .unwrap_or_else(|| {
                lanes.push(row.commit.id.clone());
                lanes.len() - 1
            });
        row.node = lanes
            .iter()
            .enumerate()
            .map(|(index, _)| if index == column { "● " } else { "│ " })
            .collect();
        let before = lanes.clone();
        lanes.remove(column);
        for (offset, parent) in row.commit.parents.iter().enumerate() {
            if !lanes.contains(parent) {
                lanes.insert((column + offset).min(lanes.len()), parent.clone());
            }
        }
        let mut cells = vec![0u8; before.len().max(lanes.len()) * 2];
        let mut connect = |from: usize, to: usize| {
            let (a, b) = (from * 2, to * 2);
            cells[a] |= 1;
            cells[b] |= 2;
            for x in a.min(b)..a.max(b) {
                cells[x] |= 8;
                cells[x + 1] |= 4;
            }
        };
        for (index, id) in before.iter().enumerate() {
            if index == column {
                for parent in &row.commit.parents {
                    if let Some(next) = lanes.iter().position(|id| id == parent) {
                        connect(index, next);
                    }
                }
            } else if let Some(next) = lanes.iter().position(|next| next == id) {
                connect(index, next);
            }
        }
        row.edges = cells
            .into_iter()
            .map(|mask| match mask {
                0 => ' ',
                1..=3 => '│',
                4 | 8 | 12 => '─',
                5 => '┘',
                6 => '┐',
                9 => '└',
                10 => '┌',
                7 => '┤',
                11 => '├',
                13 => '┴',
                14 => '┬',
                _ => '┼',
            })
            .collect();
    }
}

pub(super) fn render(
    buffer: &mut Buffer,
    area: Rect,
    graph: &mut GitGraphState,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    if area.is_empty() {
        return;
    }
    let palette = &config.palette;
    put_text(
        buffer,
        area.x,
        area.y,
        area.width,
        &"─".repeat(usize::from(area.width)),
        Style::default().fg(palette.surface_dim),
    );
    if area.height < 2 {
        return;
    }
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        " graph",
        Style::default()
            .fg(palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );
    let body = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height.saturating_sub(2),
    );
    hits.graph_body = body;
    if body.is_empty() {
        return;
    }
    if graph.rows.is_empty() {
        let message = graph
            .message
            .as_deref()
            .unwrap_or(if graph.target.is_none() {
                "No repository selected"
            } else if graph.next_refresh.is_some() && !graph.pending {
                "No commits yet"
            } else {
                "Loading…"
            });
        put_text(
            buffer,
            body.x + 1,
            body.y,
            body.width.saturating_sub(1),
            &truncate_end(message, usize::from(body.width.saturating_sub(1))),
            Style::default().fg(palette.overlay0),
        );
        return;
    }
    let visible = usize::from(body.height / 2).max(1);
    let max_scroll = graph.rows.len().saturating_sub(visible);
    graph.scroll = graph.scroll.min(max_scroll);
    hits.graph_max_scroll = max_scroll;
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll - graph.scroll,
        max_offset_from_bottom: max_scroll,
        viewport_rows: visible,
    };
    hits.graph_scroll_metrics = Some(metrics);
    let width = body.width.saturating_sub(u16::from(max_scroll > 0));
    for (index, row) in graph
        .rows
        .iter()
        .skip(graph.scroll)
        .take(visible)
        .enumerate()
    {
        let y = body.y + index as u16 * 2;
        let prefix_width = row.node.width().min(usize::from(width / 3)) as u16;
        put_text(
            buffer,
            body.x,
            y,
            prefix_width,
            &row.node,
            Style::default().fg(palette.accent),
        );
        let text_x = body.x + prefix_width;
        let text_width = width.saturating_sub(prefix_width);
        let short_id = row.commit.id.get(..7).unwrap_or(&row.commit.id);
        put_text(
            buffer,
            text_x,
            y,
            text_width.min(7),
            short_id,
            Style::default().fg(palette.mauve),
        );
        if text_width > 8 {
            put_text(
                buffer,
                text_x + 8,
                y,
                text_width - 8,
                &truncate_end(&row.commit.subject, usize::from(text_width - 8)),
                Style::default().fg(palette.subtext0),
            );
        }
        if y + 1 >= body.bottom() {
            continue;
        }
        put_text(
            buffer,
            body.x,
            y + 1,
            prefix_width,
            &row.edges,
            Style::default().fg(palette.accent),
        );
        let copy = Rect::new(text_x, y + 1, text_width.min(2), 1);
        put_text(
            buffer,
            copy.x,
            copy.y,
            copy.width,
            "⧉",
            Style::default().fg(palette.overlay1),
        );
        if config.mouse_capture && copy.width > 0 {
            hits.graph_copy.push((copy, row.commit.id.clone()));
        }
        let stats = if row.commit.binary_files > 0 {
            format!("{} binary", row.commit.binary_files)
        } else {
            format!("+{} -{}", row.commit.additions, row.commit.deletions)
        };
        if text_width > 4 {
            put_text(
                buffer,
                text_x + 4,
                y + 1,
                text_width - 4,
                &stats,
                Style::default().fg(palette.overlay0),
            );
        }
    }
    if max_scroll > 0 && body.width > 1 {
        hits.graph_scrollbar = Rect::new(body.right() - 1, body.y, 1, body.height);
        super::scroll::render_list_scrollbar(buffer, hits.graph_scrollbar, metrics, palette);
    }
}
