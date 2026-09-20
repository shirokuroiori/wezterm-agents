use super::*;

#[test]
fn tool_summary_matches_the_old_jq_program() {
    assert_eq!(
        tool_summary("Bash", &json!({ "command": "ls -la\necho second" })),
        "Bash: ls -la"
    );
    assert_eq!(
        tool_summary("Edit", &json!({ "file_path": "/tmp/a.rs", "old_string": "x" })),
        "Edit: /tmp/a.rs"
    );
    assert_eq!(
        tool_summary("NotebookEdit", &json!({ "notebook_path": "/tmp/n.ipynb" })),
        "NotebookEdit: /tmp/n.ipynb"
    );
    assert_eq!(
        tool_summary("ExitPlanMode", &json!({ "plan": "# 計画\n本文" })),
        "プラン承認待ち: # 計画"
    );
    assert_eq!(
        tool_summary(
            "AskUserQuestion",
            &json!({ "questions": [{ "question": "A?" }, { "question": "B?" }] })
        ),
        "A? / B?"
    );
    // An unknown tool falls back to the default message
    assert_eq!(tool_summary("Glob", &json!({})), "Glob の許可待ち");
    // Missing required fields also fall back to the default message
    assert_eq!(tool_summary("Bash", &json!({})), "Bash の許可待ち");
}

#[test]
fn tool_summary_is_clipped_by_codepoints() {
    let long = "あ".repeat(200);
    let s = tool_summary("Bash", &json!({ "command": long }));
    // "Bash: " (6) + 74 chars + "…" = 81 code points
    assert_eq!(s.chars().count(), SUMMARY_MAX + 1);
    assert!(s.ends_with('…'));
}

#[test]
fn tool_from_payload_accepts_both_field_namings() {
    let claude = json!({ "tool_name": "Bash", "tool_input": { "command": "ls" } });
    let (n, i) = tool_from_payload(&claude).unwrap();
    assert_eq!(n, "Bash");
    assert_eq!(i.get("command").unwrap(), "ls");

    let copilot = json!({ "toolName": "ask_user", "toolInput": { "question": "続けますか" } });
    let (n, i) = tool_from_payload(&copilot).unwrap();
    assert_eq!(n, "ask_user");
    assert_eq!(i.get("question").unwrap(), "続けますか");

    assert!(tool_from_payload(&json!({})).is_none());
    assert!(tool_from_payload(&json!({ "tool_name": "" })).is_none());
}

#[test]
fn transcript_extract_takes_the_last_text_and_tool_use() {
    let dir = std::env::temp_dir().join(format!("wa-transcript-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.jsonl");
    let body = [
        json!({"type": "user", "message": {"content": "hi"}}).to_string(),
        json!({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "古い応答"}
        ]}})
        .to_string(),
        json!({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "新しい応答\n2行目"},
            {"type": "tool_use", "name": "Bash", "input": {"command": "make"}}
        ]}})
        .to_string(),
    ]
    .join("\n");
    std::fs::write(&path, body).unwrap();

    let e = extract_from_transcript(&path);
    assert_eq!(e.text.as_deref(), Some("新しい応答\n2行目"));
    let (name, input) = e.tool.unwrap();
    assert_eq!(name, "Bash");
    assert_eq!(input.get("command").unwrap(), "make");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_tail_drops_the_partial_first_line() {
    let dir = std::env::temp_dir().join(format!("wa-tail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.txt");
    std::fs::write(&path, "aaaa\nbbbb\ncccc\n").unwrap();
    // Last 10 bytes = starts mid-way through "bb\ncccc\n". The first,
    // truncated line is discarded
    assert_eq!(read_tail(&path, 10).unwrap(), "cccc\n");
    // Returns the whole thing if it all fits
    assert_eq!(read_tail(&path, 1024).unwrap(), "aaaa\nbbbb\ncccc\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn log_line_goes_right_after_the_heading_without_doubling_blanks() {
    let body = "---\ntab_id: 1\n---\n\n# メモ\n\n## ログ\n\n- 古い行\n";
    let got = insert_log_line(body, "- 新しい行");
    assert_eq!(
        got,
        "---\ntab_id: 1\n---\n\n# メモ\n\n## ログ\n\n- 新しい行\n- 古い行\n"
    );
}

#[test]
fn log_section_is_created_when_missing() {
    let got = insert_log_line("# メモ\n\n自由記述\n", "- 追記");
    assert_eq!(got, "# メモ\n\n自由記述\n\n## ログ\n\n- 追記\n");
}
