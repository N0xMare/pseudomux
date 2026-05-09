use pseudomux_core::input::{
    KeyEvent, KittyKeyboardMode, TerminalState, encode_key, encode_text, parse_key_name,
};

fn legacy_state() -> TerminalState {
    TerminalState::default()
}

fn kitty_state() -> TerminalState {
    let mut s = TerminalState::default();
    s.keyboard_mode = KittyKeyboardMode::Kitty(1);
    s
}

#[test]
fn test_legacy_enter() {
    assert_eq!(encode_key(KeyEvent::Enter, &legacy_state()), vec![0x0d]);
}

#[test]
fn test_legacy_ctrl_c() {
    assert_eq!(encode_key(KeyEvent::Ctrl('c'), &legacy_state()), vec![0x03]);
}

#[test]
fn test_legacy_ctrl_s() {
    assert_eq!(encode_key(KeyEvent::Ctrl('s'), &legacy_state()), vec![0x13]);
}

#[test]
fn test_legacy_alt_x() {
    assert_eq!(
        encode_key(KeyEvent::Alt('x'), &legacy_state()),
        vec![0x1b, b'x']
    );
}

#[test]
fn test_legacy_arrows() {
    assert_eq!(encode_key(KeyEvent::Up, &legacy_state()), b"\x1b[A");
    assert_eq!(encode_key(KeyEvent::Down, &legacy_state()), b"\x1b[B");
    assert_eq!(encode_key(KeyEvent::Right, &legacy_state()), b"\x1b[C");
    assert_eq!(encode_key(KeyEvent::Left, &legacy_state()), b"\x1b[D");
}

#[test]
fn test_legacy_function_keys() {
    assert_eq!(
        encode_key(KeyEvent::Function(1), &legacy_state()),
        b"\x1bOP"
    );
    assert_eq!(
        encode_key(KeyEvent::Function(5), &legacy_state()),
        b"\x1b[15~"
    );
    assert_eq!(
        encode_key(KeyEvent::Function(12), &legacy_state()),
        b"\x1b[24~"
    );
}

#[test]
fn test_kitty_enter() {
    assert_eq!(encode_key(KeyEvent::Enter, &kitty_state()), b"\x1b[13u");
}

#[test]
fn test_kitty_ctrl_c() {
    assert_eq!(
        encode_key(KeyEvent::Ctrl('c'), &kitty_state()),
        b"\x1b[99;5u"
    );
}

#[test]
fn test_kitty_alt_x() {
    assert_eq!(
        encode_key(KeyEvent::Alt('x'), &kitty_state()),
        b"\x1b[120;3u"
    );
}

#[test]
fn test_kitty_ctrl_alt() {
    assert_eq!(
        encode_key(KeyEvent::CtrlAlt('c'), &kitty_state()),
        b"\x1b[99;7u"
    );
}

#[test]
fn test_encode_text_plain() {
    let s = legacy_state();
    assert_eq!(encode_text("hello", &s), b"hello");
}

#[test]
fn test_encode_text_bracketed() {
    let mut s = legacy_state();
    s.bracketed_paste = true;
    let result = encode_text("hello", &s);
    let mut expected = Vec::new();
    expected.extend_from_slice(b"\x1b[200~");
    expected.extend_from_slice(b"hello");
    expected.extend_from_slice(b"\x1b[201~");
    assert_eq!(result, expected);
}

#[test]
fn test_parse_key_name_roundtrip() {
    assert_eq!(parse_key_name("enter").unwrap(), KeyEvent::Enter);
    assert_eq!(parse_key_name("tab").unwrap(), KeyEvent::Tab);
    assert_eq!(parse_key_name("esc").unwrap(), KeyEvent::Escape);
    assert_eq!(parse_key_name("escape").unwrap(), KeyEvent::Escape);
    assert_eq!(parse_key_name("backspace").unwrap(), KeyEvent::Backspace);
    assert_eq!(parse_key_name("delete").unwrap(), KeyEvent::Delete);
    assert_eq!(parse_key_name("del").unwrap(), KeyEvent::Delete);
    assert_eq!(parse_key_name("up").unwrap(), KeyEvent::Up);
    assert_eq!(parse_key_name("f1").unwrap(), KeyEvent::Function(1));
    assert_eq!(parse_key_name("f12").unwrap(), KeyEvent::Function(12));
    assert_eq!(parse_key_name("a").unwrap(), KeyEvent::Char('a'));
    assert_eq!(parse_key_name("pgup").unwrap(), KeyEvent::PageUp);
    assert_eq!(parse_key_name("pgdn").unwrap(), KeyEvent::PageDown);
}

#[test]
fn test_parse_key_name_modifiers() {
    assert_eq!(parse_key_name("ctrl+s").unwrap(), KeyEvent::Ctrl('s'));
    assert_eq!(parse_key_name("alt+x").unwrap(), KeyEvent::Alt('x'));
    assert_eq!(
        parse_key_name("ctrl+alt+c").unwrap(),
        KeyEvent::CtrlAlt('c')
    );
}
