use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

/// Each test uses its own private temp directory. Combines std::env::temp_dir()
/// + process ID + a per-test counter into a name that won't collide even
/// under parallel execution.
fn tmp_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "wezterm-agents-memo-test-{}-{}-{}",
        std::process::id(),
        label,
        nanos
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn ensure_file_creates_frontmatter_and_sections() {
    let dir = tmp_dir("ensure");
    let path = ensure_file_in(&dir, 3, "/home/user/project").unwrap();
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.starts_with("---\ntab_id: 3\n"));
    assert!(body.contains("cwd: /home/user/project\n"));
    assert!(body.contains("# メモ"));
    assert!(body.contains("## ログ"));

    // Don't overwrite if it already exists
    fs::write(&path, "changed").unwrap();
    ensure_file_in(&dir, 3, "/home/user/project").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "changed");
}

#[test]
fn read_preview_extracts_only_memo_section() {
    let dir = tmp_dir("preview");
    fs::write(
        dir.join("tab-5.md"),
        "---\ntab_id: 5\ncwd: /x\ncreated_at: t\n---\n\n\
         # メモ\n\n- [x] a\n- [ ] b\n\n\
         ## ログ\n\n- 2026-08-16 10:00 claude: 何か\n",
    )
    .unwrap();
    let preview = read_preview_in(&dir, 5).unwrap();
    assert_eq!(preview, "- [x] a\n- [ ] b");
    assert!(!preview.contains("ログ"));
    assert!(!preview.contains("claude"));
}

#[test]
fn read_preview_none_when_memo_section_empty_or_missing() {
    let dir = tmp_dir("preview-empty");
    assert_eq!(read_preview_in(&dir, 1), None); // file doesn't exist

    fs::write(dir.join("tab-2.md"), "---\ntab_id: 2\ncwd: /x\ncreated_at: t\n---\n\n# メモ\n\n## ログ\n\n- x\n").unwrap();
    assert_eq!(read_preview_in(&dir, 2), None); // empty
}

#[test]
fn gc_stale_archives_missing_and_mismatched_cwd() {
    let dir = tmp_dir("gc");
    // tab 1: still exists and cwd matches -> stays
    fs::write(dir.join("tab-1.md"), "---\ntab_id: 1\ncwd: /a\ncreated_at: t1\n---\n\n# メモ\n").unwrap();
    // tab 2: still exists but cwd differs -> archived
    fs::write(dir.join("tab-2.md"), "---\ntab_id: 2\ncwd: /old\ncreated_at: t2\n---\n\n# メモ\n").unwrap();
    // tab 3: tab no longer exists -> archived
    fs::write(dir.join("tab-3.md"), "---\ntab_id: 3\ncwd: /c\ncreated_at: t3\n---\n\n# メモ\n").unwrap();

    let live = vec![(1u64, "/a".to_string()), (2u64, "/new".to_string())];
    gc_stale_in(&dir, &live);

    assert!(dir.join("tab-1.md").exists(), "a matching one should stay");
    assert!(!dir.join("tab-2.md").exists(), "a cwd mismatch should be archived");
    assert!(!dir.join("tab-3.md").exists(), "a nonexistent tab should be archived");

    let archived: Vec<_> = fs::read_dir(archive_dir_in(&dir))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(archived.len(), 2);
    assert!(archived.iter().any(|n| n.starts_with("tab-2-")));
    assert!(archived.iter().any(|n| n.starts_with("tab-3-")));
}

#[test]
fn gc_stale_is_noop_on_missing_dir() {
    // The call itself must not fail (must not panic even on a nonexistent directory)
    let dir = std::env::temp_dir().join("wezterm-agents-memo-test-does-not-exist");
    gc_stale_in(&dir, &[]);
}
