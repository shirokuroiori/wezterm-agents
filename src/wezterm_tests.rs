use super::*;

#[test]
fn base64_matches_reference() {
    assert_eq!(b64(b"14"), "MTQ=");
    assert_eq!(b64(b"7"), "Nw==");
    assert_eq!(b64(b"123"), "MTIz");
    assert_eq!(b64(b"1234"), "MTIzNA==");
}

#[test]
fn cwd_is_decoded() {
    assert_eq!(decode_cwd("file:///Users/example/dotfiles/"), "/Users/example/dotfiles");
    assert_eq!(decode_cwd("file:///Users/example/my%20dir"), "/Users/example/my dir");
    assert_eq!(decode_cwd("file:///"), "/");
}
