use super::*;

#[test]
fn detects_both_spinner_styles() {
    // Braille spinner (older Claude Code)
    assert!(is_working_title("\u{28C0} 何かの要約"));
    // Circular spinner (current Claude Code; confirmed in a real pane title: ◐)
    assert!(is_working_title("◐ Wezterm マルチエージェント計画の要件分析"));
    assert!(is_working_title("◓ x"));
    assert!(is_working_title("◑ x"));
    assert!(is_working_title("◒ x"));
    // Visually similar symbols (◎●○◉) must not false-positive
    assert!(!is_working_title("◎ x"));
    assert!(!is_working_title("● x"));
    assert!(!is_working_title("nvim"));
    assert!(!is_working_title(""));
}

#[test]
fn task_from_title_strips_both_spinner_styles() {
    assert_eq!(task_from_title("◐ Wezterm マルチエージェント計画の要件分析"), "Wezterm マルチエージェント計画の要件分析");
    assert_eq!(task_from_title("\u{28C0} タスクの要約"), "タスクの要約");
    assert_eq!(task_from_title("✳ 通常時のタイトル"), "通常時のタイトル");
}

#[test]
fn agent_from_title_reads_foreground_process_name() {
    assert_eq!(agent_from_title("nvim"), Some("nvim".to_string()));
    assert_eq!(agent_from_title("vim"), Some("vim".to_string()));
    assert_eq!(agent_from_title("zsh"), Some("zsh".to_string()));
    assert_eq!(agent_from_title("bash"), Some("bash".to_string()));
    // Nothing informative stays None (left to display as "-")
    assert_eq!(agent_from_title(""), None);
    assert_eq!(agent_from_title("wezterm-gui"), None);
}

#[test]
fn agent_from_title_recognizes_claude_marker_instead_of_task_text() {
    // What follows the spinner/✳ is task wording, not a process name, so
    // we don't emit the stripped remainder as-is — we report "claude".
    assert_eq!(agent_from_title("◐ 動画編集の自動化"), Some("claude".to_string()));
    assert_eq!(agent_from_title("\u{28C0} タスクの要約"), Some("claude".to_string()));
    assert_eq!(agent_from_title("✳ 動画編集の自動化"), Some("claude".to_string()));
}

fn notif(at: &str, kind: &str) -> Notification {
    Notification {
        at: at.to_string(),
        agent: "copilot".to_string(),
        kind: kind.to_string(),
        text: String::new(),
        task: String::new(),
        unread: false,
    }
}

#[test]
fn working_since_wins_when_no_notification_yet() {
    // No notifications at all = the first send. working_since alone
    // determines the working state.
    assert_eq!(
        derive_state(false, Some("2026-08-17T10:00:00+09:00"), &[]),
        State::Working
    );
}

#[test]
fn working_since_wins_when_newer_than_last_notification() {
    let notifications = [notif("2026-08-17T09:00:00+09:00", "done")];
    // Sent after the previous done = still generating a response
    assert_eq!(
        derive_state(false, Some("2026-08-17T10:00:00+09:00"), &notifications),
        State::Working
    );
}

#[test]
fn notification_supersedes_stale_working_since() {
    let notifications = [notif("2026-08-17T10:00:00+09:00", "done")];
    // done arrived after working_since = that turn has already ended
    assert_eq!(
        derive_state(false, Some("2026-08-17T09:00:00+09:00"), &notifications),
        State::Idle
    );
    let waiting = [notif("2026-08-17T10:00:00+09:00", "waiting")];
    assert_eq!(
        derive_state(false, Some("2026-08-17T09:00:00+09:00"), &waiting),
        State::Waiting
    );
}

#[test]
fn no_working_since_falls_back_to_notifications() {
    let notifications = [notif("2026-08-17T10:00:00+09:00", "waiting")];
    assert_eq!(derive_state(false, None, &notifications), State::Waiting);
    assert_eq!(derive_state(false, None, &[]), State::Idle);
}
