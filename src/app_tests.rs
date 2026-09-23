use super::*;
use crate::model::{Group, Pane, Snapshot, State};

fn pane(pane_id: u64, tab_id: u64) -> Pane {
    Pane {
        pane_id,
        window_id: 1,
        tab_id,
        workspace: "default".to_string(),
        cwd: "/tmp".to_string(),
        branch: None,
        agent: None,
        state: State::Idle,
        unread: 0,
        notifications: Vec::new(),
        task: "-".to_string(),
    }
}

fn app_with_origin(origin_pane: Option<u64>) -> App {
    App {
        snapshot: Snapshot::default(),
        rows: Vec::new(),
        cursor: 0,
        filter: String::new(),
        filter_active: false,
        message: None,
        layout: LayoutMode::Split,
        should_quit: false,
        exit_on_jump: false,
        pending_exit: None,
        pending_edit: None,
        store: Store::new(),
        self_pane: None,
        origin_pane,
    }
}

#[test]
fn initial_cursor_lands_on_origin_tab_not_first_group() {
    let mut app = app_with_origin(Some(20));
    app.snapshot = Snapshot {
        groups: vec![
            Group { cwd: "/a".to_string(), panes: vec![pane(10, 1)] },
            Group { cwd: "/b".to_string(), panes: vec![pane(20, 2)] },
        ],
    };
    app.rebuild_rows();

    assert_eq!(app.selected_pane().map(|p| p.pane_id), Some(20));
}

#[test]
fn initial_cursor_uses_topmost_pane_of_a_split_origin_tab() {
    // Origin pane is the second (bottom) split of tab 1; the cursor should
    // still land on the topmost pane of that same tab, not on pane 11
    // specifically.
    let mut app = app_with_origin(Some(11));
    app.snapshot = Snapshot {
        groups: vec![Group {
            cwd: "/a".to_string(),
            panes: vec![pane(10, 1), pane(11, 1)],
        }],
    };
    app.rebuild_rows();

    assert_eq!(app.selected_pane().map(|p| p.pane_id), Some(10));
}

#[test]
fn falls_back_to_first_pane_when_origin_pane_is_absent() {
    // e.g. `--watch`, where there's no origin_pane at all.
    let mut app = app_with_origin(None);
    app.snapshot = Snapshot {
        groups: vec![Group { cwd: "/a".to_string(), panes: vec![pane(10, 1)] }],
    };
    app.rebuild_rows();

    assert_eq!(app.selected_pane().map(|p| p.pane_id), Some(10));
}

#[test]
fn falls_back_to_first_pane_when_origin_pane_not_in_snapshot() {
    // The pane that was active before cmd+shift+a isn't a tracked agent
    // pane, so it never shows up in the snapshot.
    let mut app = app_with_origin(Some(999));
    app.snapshot = Snapshot {
        groups: vec![Group { cwd: "/a".to_string(), panes: vec![pane(10, 1)] }],
    };
    app.rebuild_rows();

    assert_eq!(app.selected_pane().map(|p| p.pane_id), Some(10));
}

#[test]
fn origin_tab_preference_only_applies_on_the_first_rebuild() {
    // Once the cursor has a real anchor, later rebuilds must keep following
    // it rather than snapping back to the origin tab.
    let mut app = app_with_origin(Some(20));
    app.snapshot = Snapshot {
        groups: vec![
            Group { cwd: "/a".to_string(), panes: vec![pane(10, 1)] },
            Group { cwd: "/b".to_string(), panes: vec![pane(20, 2)] },
        ],
    };
    app.rebuild_rows();
    assert_eq!(app.selected_pane().map(|p| p.pane_id), Some(20));

    // User moves the cursor to pane 10.
    app.move_cursor(-1);
    assert_eq!(app.selected_pane().map(|p| p.pane_id), Some(10));

    // A later refresh (e.g. the 1s tick) must not reset it back to 20.
    app.rebuild_rows();
    assert_eq!(app.selected_pane().map(|p| p.pane_id), Some(10));
}
