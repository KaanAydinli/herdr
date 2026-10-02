use super::*;
use crate::api::schema::{GitCommit, GitHistory, ResponseResult};
use std::time::{Duration, Instant};

fn state() -> ClientShellState {
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.mobile_width_threshold = 0;
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_endpoint_methods(Some(vec!["git.history".into()]));
    state.set_pane_surface(surface());
    state.compose(106, 30).unwrap();
    state
}

fn mouse(kind: MouseEventKind, rect: Rect) -> RawInputEvent {
    RawInputEvent::Mouse(MouseEvent {
        kind,
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::empty(),
    })
}

fn history(count: usize) -> GitHistory {
    let ids = (0..count)
        .map(|index| format!("{index:07x}{}", "a".repeat(33)))
        .collect::<Vec<_>>();
    GitHistory {
        head: ids.first().cloned(),
        commits: ids
            .iter()
            .enumerate()
            .map(|(index, id)| GitCommit {
                id: id.clone(),
                parents: ids.get(index + 1).cloned().into_iter().collect(),
                subject: format!("change {index} 日本語"),
                additions: 7,
                deletions: 3,
                binary_files: 0,
            })
            .collect(),
        has_more: false,
        unchanged: false,
    }
}

fn request(state: &mut ClientShellState, now: Instant) -> String {
    let mut input = ClientShellInput::default();
    state.tick_git_graph(now, &mut input);
    let [ClientShellAction::Endpoint { request, .. }] = &input.actions[..] else {
        panic!("expected history request")
    };
    assert!(matches!(
        request.method,
        crate::api::schema::Method::GitHistory(_)
    ));
    request.id.clone()
}

fn populate(state: &mut ClientShellState, count: usize) {
    let id = request(state, Instant::now());
    let (repaint, actions) = state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(ResponseResult::GitHistory {
            history: history(count),
        }),
    );
    assert!(repaint);
    assert!(actions.is_empty());
    state.compose(106, 30).unwrap();
}

#[test]
fn sidebar_sections_follow_content_and_new_space_is_a_centered_tile() {
    let mut state = state();
    let frame = state.compose(106, 40).unwrap();
    let buffer = frame.to_ratatui_buffer().unwrap();
    let tile = state.hits.new_workspace;
    let last = state.hits.workspaces.last().unwrap().rect;
    assert_eq!(tile.y, last.bottom() + state.config.spaces.row_gap);
    assert_eq!(tile.height, 1);
    assert_eq!(
        buffer[(
            tile.x + (tile.width - 1) / 2,
            tile.y + (tile.height - 1) / 2
        )]
            .symbol(),
        "+"
    );
    assert_eq!(state.hits.global_launcher.y, tile.bottom());
    assert_eq!(state.hits.agent_body.y, tile.bottom() + 4);
    assert_eq!(state.hits.graph_body.y, state.hits.agent_body.bottom() + 2);
    let before = state.hits.graph_body.y;
    let mut snapshot = snapshot();
    let mut next = snapshot.workspaces[0].clone();
    next.workspace_id = "ws_added".into();
    next.focused = false;
    snapshot.workspaces.push(next);
    state.set_snapshot(Box::new(snapshot));
    state.compose(106, 40).unwrap();
    assert!(state.hits.graph_body.y > before);
    state.set_snapshot(Box::new(super::snapshot()));
    state.compose(106, 40).unwrap();
    assert_eq!(state.hits.graph_body.y, before);
    assert!(state
        .hits
        .workspaces
        .iter()
        .all(|hit| hit.workspace_id != "ws_added"));
}

#[test]
fn sidebar_graph_scroll_copy_and_resize_use_current_visible_hits() {
    let mut state = state();
    populate(&mut state, 40);
    let old_id = state.hits.graph_copy[0].1.clone();
    let workspace_scroll = state.workspace_scroll;
    let agent_scroll = state.agent_scroll;
    let input = state.handle_raw_events(vec![mouse(
        MouseEventKind::ScrollDown,
        state.hits.graph_body,
    )]);
    assert!(input.repaint);
    state.compose(106, 30).unwrap();
    assert_eq!(state.git_graph.scroll, 1);
    assert_eq!(state.workspace_scroll, workspace_scroll);
    assert_eq!(state.agent_scroll, agent_scroll);
    let (copy, id) = state.hits.graph_copy[0].clone();
    assert_ne!(id, old_id);
    let input = state.handle_raw_events(vec![mouse(MouseEventKind::Down(MouseButton::Left), copy)]);
    assert!(
        matches!(&input.actions[..], [ClientShellAction::ClipboardWrite(bytes)] if bytes == id.as_bytes())
    );
    for rows in [12, 4, 0, 1, 30, 60, 16] {
        if let Some(frame) = state.compose(106, rows) {
            assert!(frame.to_ratatui_buffer().is_some());
            for (rect, _) in &state.hits.graph_copy {
                assert!(rect.bottom() <= rows);
                assert!(rect.y >= state.hits.graph_body.y);
            }
        }
    }
    state.sidebar_collapsed = true;
    state.compose(106, 30).unwrap();
    assert!(state.hits.graph_copy.is_empty());
    assert!(state.hits.graph_body.is_empty());
    let mut input = ClientShellInput::default();
    state.tick_git_graph(Instant::now() + Duration::from_secs(60), &mut input);
    assert!(input.actions.is_empty());
}

#[test]
fn sidebar_graph_rejects_delayed_repository_results_and_legacy_servers() {
    let mut state = state();
    let old = request(&mut state, Instant::now());
    let mut changed = snapshot();
    changed.panes[0].cwd = Some("/new-repo".into());
    changed.panes[0].foreground_cwd = Some("/new-repo".into());
    state.set_snapshot(Box::new(changed));
    state.handle_endpoint_result(
        "boot-1",
        &old,
        Ok(ResponseResult::GitHistory {
            history: history(20),
        }),
    );
    assert!(state.git_graph.rows.is_empty());
    state.set_endpoint_methods(Some(Vec::new()));
    state.compose(106, 30).unwrap();
    let mut input = ClientShellInput::default();
    state.tick_git_graph(Instant::now(), &mut input);
    assert!(input.actions.is_empty());
    assert!(state.visible_endpoint_notice.is_none());
    assert_eq!(
        state.git_graph.message.as_deref(),
        Some("History unavailable on this server")
    );
}

#[test]
fn sidebar_graph_requests_are_throttled_and_render_never_starts_io() {
    let mut state = state();
    populate(&mut state, 20);
    for _ in 0..10 {
        state.compose(106, 30).unwrap();
    }
    assert!(state.pending_requests.is_empty());
    let mut input = ClientShellInput::default();
    state.tick_git_graph(Instant::now(), &mut input);
    assert!(input.actions.is_empty());
    let id = request(&mut state, Instant::now() + Duration::from_secs(11));
    state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(ResponseResult::GitHistory {
            history: GitHistory {
                head: history(20).head,
                commits: Vec::new(),
                has_more: false,
                unchanged: true,
            },
        }),
    );
    assert_eq!(state.git_graph.rows.len(), 20);
}

#[test]
fn sidebar_short_terminal_keeps_spaces_and_controls_reachable() {
    let mut state = state();
    for height in 10..=16 {
        state.compose(80, height).unwrap();
        assert!(!state.hits.workspace_body.is_empty(), "height={height}");
        assert!(!state.hits.workspaces.is_empty(), "height={height}");
        assert!(!state.hits.new_workspace.is_empty(), "height={height}");
        assert!(!state.hits.global_launcher.is_empty(), "height={height}");
        assert!(state.hits.new_workspace.bottom() <= height);
    }
}

#[test]
fn sidebar_graph_recovers_scrolled_history_and_throttles_failed_pages() {
    let mut state = state();
    let id = request(&mut state, Instant::now());
    let mut page = history(100);
    page.has_more = true;
    state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(ResponseResult::GitHistory { history: page }),
    );
    state.compose(106, 30).unwrap();
    state.git_graph.scroll = 99;
    let page_request = request(&mut state, Instant::now());
    state.handle_endpoint_result(
        "boot-1",
        &page_request,
        Err(ClientShellEndpointError {
            code: Some("git_history_timeout".into()),
            message: "Git history timed out".into(),
        }),
    );
    let mut input = ClientShellInput::default();
    state.tick_git_graph(Instant::now(), &mut input);
    assert!(input.actions.is_empty());
    state.compose(106, 30).unwrap();
    assert!(!state.hits.graph_copy.is_empty());
    state.git_graph.scroll = 10;
    let recovery = request(&mut state, Instant::now() + Duration::from_secs(11));
    state.handle_endpoint_result(
        "boot-1",
        &recovery,
        Ok(ResponseResult::GitHistory {
            history: GitHistory {
                head: history(100).head,
                commits: Vec::new(),
                has_more: true,
                unchanged: true,
            },
        }),
    );
    assert!(state.git_graph.message.is_none());
    assert_eq!(state.git_graph.scroll, 10);
    assert_eq!(state.git_graph.rows.len(), 100);
}
