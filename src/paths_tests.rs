use super::*;

/// Tests don't touch `status_dir()` (one per process); they only exercise functions
/// that take an explicit path. `cargo test` runs in parallel, so depending on env vars
/// or process-shared state would let tests stomp on each other.
fn tmp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wezterm-agents-test-{}-{}-{}",
        label,
        std::process::id(),
        crate::util::epoch_secs()
    ));
    let _ = fs::remove_dir_all(&dir);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .unwrap();
    dir
}

#[test]
fn verify_accepts_a_private_dir_we_own() {
    let dir = tmp_dir("verify-ok");
    assert!(verify(&dir, Lang::En).is_ok());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn verify_rejects_a_group_or_world_accessible_dir() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp_dir("verify-perm");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let err = verify(&dir, Lang::En).unwrap_err();
    assert!(err.contains("accessible by other users"), "{err}");
    let err_ja = verify(&dir, Lang::Ja).unwrap_err();
    assert!(err_ja.contains("他ユーザーからアクセス可能"), "{err_ja}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn verify_rejects_a_symlink() {
    let dir = tmp_dir("verify-symlink");
    let real = dir.join("real");
    let link = dir.join("link");
    DirBuilder::new().mode(0o700).create(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let err = verify(&link, Lang::En).unwrap_err();
    assert!(err.contains("symlink"), "{err}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn private_writes_are_0600_and_refuse_symlinks() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp_dir("write-private");

    let path = dir.join("plain");
    write_private(&path, "hello").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "mode={mode:o}");

    // Doesn't write to a pre-planted symlink
    let victim = dir.join("victim");
    fs::write(&victim, "important").unwrap();
    let planted = dir.join("planted");
    std::os::unix::fs::symlink(&victim, &planted).unwrap();
    assert!(write_private(&planted, "clobbered").is_err());
    assert!(append_private(&planted, "clobbered").is_err());
    assert_eq!(fs::read_to_string(&victim).unwrap(), "important");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn append_private_creates_then_appends() {
    let dir = tmp_dir("append");
    let path = dir.join("log.jsonl");
    append_private(&path, "a\n").unwrap();
    append_private(&path, "b\n").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "a\nb\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn replace_atomically_leaves_no_temp_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmp_dir("replace");
    let path = dir.join("state.json");
    replace_atomically(&path, "one").unwrap();
    replace_atomically(&path, "two").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "two");
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "mode={mode:o}");
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp."))
        .collect();
    assert!(leftovers.is_empty(), "残骸: {leftovers:?}");
    let _ = fs::remove_dir_all(&dir);
}
