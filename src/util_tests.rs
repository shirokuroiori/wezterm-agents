use super::*;

#[test]
fn offset_colon_is_inserted_only_when_needed() {
    assert_eq!(
        with_offset_colon("2026-09-13T07:52:31+0900"),
        "2026-09-13T07:52:31+09:00"
    );
    assert_eq!(
        with_offset_colon("2026-09-13T07:52:31+09:00"),
        "2026-09-13T07:52:31+09:00"
    );
    assert_eq!(with_offset_colon("short"), "short");
}

#[test]
fn first_line_skips_leading_blank_lines() {
    assert_eq!(first_line("\n\nhello\nworld"), "hello");
    assert_eq!(first_line("hello"), "hello");
    assert_eq!(first_line(""), "");
}

#[test]
fn clip_counts_codepoints_not_bytes() {
    // 80 Japanese characters is 240 bytes, but it must not be cut short
    let s: String = "あ".repeat(80);
    assert_eq!(clip(&s, 80), s);
    let s81: String = "あ".repeat(81);
    assert_eq!(clip(&s81, 80).chars().count(), 81); // 80文字 + …
    assert!(clip(&s81, 80).ends_with('…'));
}
