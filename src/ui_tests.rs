use super::*;

/// fit is central to column alignment, so verify the width always comes out exact.
#[test]
fn fit_pads_and_truncates_by_display_width() {
    assert_eq!(fit("abc", 6).width(), 6);
    assert_eq!(fit("abc", 6), "abc   ");
    // CJK characters are 2 columns each. Counting by character count breaks this.
    assert_eq!(fit("あい", 6).width(), 6);
    assert_eq!(fit("あい", 6), "あい  ");
    // Doesn't truncate when it fits exactly
    assert_eq!(fit("あいう", 6), "あいう");
    // When it overflows, add … and fit exactly within the width
    assert_eq!(fit("あいうえ", 6).width(), 6);
    assert_eq!(fit("abcdefgh", 4).width(), 4);
    assert_eq!(fit("abcdefgh", 4), "abc…");
    // Doesn't exceed the width even when a 2-column character straddles the boundary
    assert_eq!(fit("あいう", 4).width(), 4);
    assert_eq!(fit("", 3), "   ");
    assert_eq!(fit("abc", 0), "");
}
