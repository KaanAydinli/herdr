use super::*;

#[tokio::test]
async fn git_history_read_does_not_claim_foreground_or_geometry() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("history")];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    let mut controls = Vec::new();
    for (client_id, cols, rows) in [(41, 80, 23), (42, 100, 40)] {
        let (writer, control_rx, _render_rx) = test_client_writer();
        server.handle_server_event(ServerEvent::ClientShellConnected {
            surface_reuse: false,
            surface_delta: false,
            surface_scroll: false,
            client_id,
            surface_cols: cols,
            surface_rows: rows,
            cell_width_px: 0,
            cell_height_px: 0,
            pixel_mouse: false,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: true,
            writer,
        });
        controls.push(control_rx);
    }
    let foreground = server.foreground_client_id;
    assert_eq!(foreground, Some(42));
    let size = server.effective_size;
    let active = server.app.state.active;
    let controllers = server.tab_geometry_controllers.clone();
    let request = api::schema::Request {
        id: "history-test".into(),
        method: api::schema::Method::GitHistory(api::schema::GitHistoryParams {
            cwd: env!("CARGO_MANIFEST_DIR").into(),
            revision: None,
            known_head: None,
            skip: 0,
            limit: 1,
        }),
    };
    assert!(!server.handle_client_shell_endpoint_request(
        41,
        server.client_shell_boot_id.clone(),
        Box::new(request)
    ));
    assert_eq!(server.foreground_client_id, foreground);
    assert_eq!(server.effective_size, size);
    assert_eq!(server.app.state.active, active);
    assert_eq!(server.tab_geometry_controllers, controllers);
    let response = tokio::time::timeout(Duration::from_secs(12), server.server_event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(!server.handle_server_event(response));
    assert!(!server.clients[&41].shell_endpoint_command_in_flight);
    assert_eq!(server.foreground_client_id, foreground);
    assert_eq!(server.effective_size, size);
    assert_eq!(server.tab_geometry_controllers, controllers);
    shutdown_test_runtimes(&mut server);
}
