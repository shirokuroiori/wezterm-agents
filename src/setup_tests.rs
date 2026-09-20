use super::*;

#[test]
fn claude_settings_json_round_trips_and_has_expected_shape() {
    let snippet = shell_init("zsh", "/usr/local/bin/wezterm-agents").unwrap();
    // Pull just the single-quoted JSON portion out of the shell function definition.
    let start = snippet.find('\'').unwrap() + 1;
    let end = snippet.rfind('\'').unwrap();
    let json_str = &snippet[start..end];

    let v: Value = serde_json::from_str(json_str).expect("生成したJSONがパースできること");
    let pretool = &v["hooks"]["PreToolUse"][0]["hooks"][0]["command"];
    assert_eq!(
        pretool.as_str().unwrap(),
        "\"/usr/local/bin/wezterm-agents\" hook --agent claude pretool"
    );
    assert_eq!(
        v["hooks"]["Notification"][0]["matcher"].as_str().unwrap(),
        "permission_prompt|agent_needs_input"
    );
}

#[test]
fn zsh_and_bash_share_the_same_wrapper_shape() {
    let bin = "/x/wezterm-agents";
    assert_eq!(shell_init("zsh", bin).unwrap(), shell_init("bash", bin).unwrap());
}

#[test]
fn fish_uses_function_syntax_not_curly_braces() {
    let s = shell_init("fish", "/x/wezterm-agents").unwrap();
    assert!(s.starts_with("function claude"));
    assert!(s.contains("$argv"));
    assert!(!s.contains("$@"));
}

#[test]
fn unknown_shell_is_rejected() {
    assert!(shell_init("powershell", "/x/wezterm-agents").is_err());
}

#[test]
fn copilot_hooks_doc_has_expected_shape() {
    let dir = std::env::temp_dir().join(format!("wa-setup-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let path = install_copilot_in("/x/wezterm-agents", &dir).unwrap();
    assert_eq!(path, dir.join("wezterm-agents.json"));

    let body = std::fs::read_to_string(&path).unwrap();
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v["hooks"]["preToolUse"][0]["bash"].as_str().unwrap(),
        "\"/x/wezterm-agents\" hook --agent copilot pretool"
    );
    assert_eq!(v["hooks"]["agentStop"][0]["bash"], v["hooks"]["sessionEnd"][0]["bash"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn same_file_follows_symlinks_and_rejects_missing() {
    let dir = std::env::temp_dir().join(format!("wa-setup-test-same-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let real = dir.join("real");
    let link = dir.join("link");
    std::fs::write(&real, "x").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    assert!(same_file(&link, &real), "symlink と実体は同じファイル");
    assert!(same_file(&real, &real));
    assert!(!same_file(&link, &dir.join("other")), "存在しないパスは false");
    std::fs::write(dir.join("other"), "y").unwrap();
    assert!(!same_file(&real, &dir.join("other")), "別ファイルは false");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn upsert_block_appends_with_blank_line_separator() {
    let block = "BEGIN\nx\nEND\n";
    // An empty file gets the block as-is.
    assert_eq!(upsert_block("", block).unwrap(), block);
    // Trailing newline present: insert one blank line.
    assert_eq!(
        upsert_block("a\n", block).unwrap(),
        "a\n\nBEGIN\nx\nEND\n"
    );
    // No trailing newline: add one, then the blank line.
    assert_eq!(
        upsert_block("a", block).unwrap(),
        "a\n\nBEGIN\nx\nEND\n"
    );
}

#[test]
fn upsert_block_replaces_existing_section_in_place() {
    let old = zshenv_block("/x/a");
    let body = format!("before\n\n{old}\nafter\n");
    let out = upsert_block(&body, &zshenv_block("/x/b")).unwrap();
    assert_eq!(out, format!("before\n\n{}\nafter\n", zshenv_block("/x/b")));
    assert!(!out.contains("/x/a"), "古いパスが残っている");
    assert_eq!(out.matches(ZSHENV_BEGIN).count(), 1, "区画が増殖している");
}

#[test]
fn upsert_block_rejects_unbalanced_markers() {
    let block = zshenv_block("/x/a");
    assert!(upsert_block(&format!("{ZSHENV_BEGIN}\nfoo\n"), &block).is_err());
    assert!(upsert_block(&format!("{ZSHENV_END}\nfoo\n"), &block).is_err());
    assert!(upsert_block(&format!("{ZSHENV_END}\n{ZSHENV_BEGIN}\n"), &block).is_err());
}

#[test]
fn zshenv_target_prefers_home_then_zdotdir_and_skips_plugin_dir() {
    let root = std::env::temp_dir().join(format!("wa-setup-test-target-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let zdot = root.join("zdot");
    let plugin = root.join("plugin");
    for d in [&home, &zdot, &plugin] {
        std::fs::create_dir_all(d).unwrap();
    }
    let h = home.to_str().unwrap();
    let z = zdot.to_str().unwrap();
    let p = plugin.to_str().unwrap();

    // No ~/.zshenv and no ZDOTDIR → ~/.zshenv (as the file to create).
    assert_eq!(zshenv_target(h, None, None), home.join(".zshenv"));
    // No ~/.zshenv but ZDOTDIR set → $ZDOTDIR/.zshenv.
    assert_eq!(zshenv_target(h, Some(z), None), zdot.join(".zshenv"));

    // ZDOTDIR is the plugin's bundled directory → fall back, or HOME if that's unset too.
    std::fs::write(plugin.join(".zshenv"), format!("{PLUGIN_ZSHENV_MARKER}\n# ...\n")).unwrap();
    assert_eq!(zshenv_target(h, Some(p), Some(z)), zdot.join(".zshenv"));
    assert_eq!(zshenv_target(h, Some(p), None), home.join(".zshenv"));
    // A .zshenv without the marker is treated as an ordinary ZDOTDIR.
    std::fs::write(plugin.join(".zshenv"), "# user's own\n").unwrap();
    assert_eq!(zshenv_target(h, Some(p), Some(z)), plugin.join(".zshenv"));

    // If ~/.zshenv exists, it takes priority over ZDOTDIR.
    std::fs::write(home.join(".zshenv"), "export ZDOTDIR=~/.config/zsh\n").unwrap();
    assert_eq!(zshenv_target(h, Some(z), None), home.join(".zshenv"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn plugin_zshenv_marker_matches_bundled_file() {
    let bundled = concat!(env!("CARGO_MANIFEST_DIR"), "/plugin/shell-integration/.zshenv");
    let body = std::fs::read_to_string(bundled).expect("同梱 .zshenv が読めること");
    assert_eq!(body.lines().next(), Some(PLUGIN_ZSHENV_MARKER), "目印の行が同梱ファイルとずれている");
}

#[test]
fn shell_bin_expr_replaces_home_prefix_only_at_path_boundary() {
    assert_eq!(
        shell_bin_expr("/home/alice/.local/bin/wezterm-agents", "/home/alice"),
        "$HOME/.local/bin/wezterm-agents"
    );
    // Same result even when HOME has a trailing slash.
    assert_eq!(shell_bin_expr("/home/alice/x", "/home/alice/"), "$HOME/x");
    // A different user's home dir (merely a prefix match) is not substituted.
    assert_eq!(shell_bin_expr("/home/alice2/x", "/home/alice"), "/home/alice2/x");
    // Outside the home dir, left unchanged.
    assert_eq!(shell_bin_expr("/opt/homebrew/bin/x", "/home/alice"), "/opt/homebrew/bin/x");
    assert_eq!(shell_bin_expr("/opt/x", ""), "/opt/x");
}

#[test]
fn zshenv_block_guards_with_absolute_path_not_command_v() {
    let block = zshenv_block("/opt/x/wezterm-agents");
    assert!(block.contains("if [ -x \"/opt/x/wezterm-agents\" ]"));
    assert!(block.contains("eval \"$(\"/opt/x/wezterm-agents\" init zsh)\""));
    assert!(!block.contains("command -v"));
    assert!(block.starts_with(ZSHENV_BEGIN));
    assert!(block.ends_with(&format!("{ZSHENV_END}\n")));
}

#[test]
fn install_claude_in_creates_updates_and_preserves_user_content() {
    let dir = std::env::temp_dir().join(format!("wa-setup-test-claude-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".zshenv");

    // A missing file gets created fresh.
    install_claude_in("/x/a", &path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), zshenv_block("/x/a"));

    // Adding a user line and re-running leaves that line intact and only updates the block.
    std::fs::write(&path, format!("export FOO=1\n{}", zshenv_block("/x/a"))).unwrap();
    install_claude_in("/x/b", &path).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.starts_with("export FOO=1\n"));
    assert!(body.contains("/x/b"));
    assert!(!body.contains("/x/a"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn install_copilot_in_is_idempotent() {
    let dir = std::env::temp_dir().join(format!("wa-setup-test-idem-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    install_copilot_in("/x/a", &dir).unwrap();
    let path = install_copilot_in("/x/b", &dir).unwrap();
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("/x/b"));
    assert!(!body.contains("/x/a"), "上書きされず古い内容が残っている");

    let _ = std::fs::remove_dir_all(&dir);
}
