use pseudomux_core::input::{
    CapabilityNegotiator, CapabilityPolicy, KeyboardPolicy, KittyKeyboardMode,
};

fn deny_negotiator() -> CapabilityNegotiator {
    CapabilityNegotiator::new(CapabilityPolicy::default())
}

fn accept_negotiator() -> CapabilityNegotiator {
    CapabilityNegotiator::new(CapabilityPolicy {
        keyboard: KeyboardPolicy::Accept,
        ..Default::default()
    })
}

#[test]
fn test_deny_kitty_query() {
    let mut n = deny_negotiator();
    let resp = n.process(b"\x1b[?u");
    assert_eq!(resp, b"\x1b[?0u");
    assert_eq!(n.state().keyboard_mode, KittyKeyboardMode::Legacy);
}

#[test]
fn test_accept_kitty_query() {
    let mut n = accept_negotiator();
    let resp = n.process(b"\x1b[?u");
    assert_eq!(resp, b"\x1b[?0u");
}

#[test]
fn test_deny_kitty_push_tracks_state() {
    let mut n = deny_negotiator();
    let _resp = n.process(b"\x1b[>5u");
    assert_eq!(n.state().keyboard_mode, KittyKeyboardMode::Kitty(5));
}

#[test]
fn test_modify_other_keys_does_not_affect_kitty() {
    let mut n = accept_negotiator();
    n.process(b"\x1b[>5u");
    assert_eq!(n.state().keyboard_mode, KittyKeyboardMode::Kitty(5));
    n.process(b"\x1b[>4;0m");
    assert_eq!(n.state().keyboard_mode, KittyKeyboardMode::Kitty(5));
}

#[test]
fn test_accept_kitty_push_pop() {
    let mut n = accept_negotiator();
    n.process(b"\x1b[>1u");
    assert_eq!(n.state().keyboard_mode, KittyKeyboardMode::Kitty(1));
    n.process(b"\x1b[<u");
    assert_eq!(n.state().keyboard_mode, KittyKeyboardMode::Legacy);
}

#[test]
fn test_kitty_push_flags_5() {
    let mut n = accept_negotiator();
    n.process(b"\x1b[>5u");
    assert_eq!(n.state().keyboard_mode, KittyKeyboardMode::Kitty(5));
}

#[test]
fn test_bracketed_paste_tracking() {
    let mut n = deny_negotiator();
    n.process(b"\x1b[?2004h");
    assert!(n.state().bracketed_paste);
    n.process(b"\x1b[?2004l");
    assert!(!n.state().bracketed_paste);
}

#[test]
fn test_da1_response() {
    let mut n = deny_negotiator();
    let resp = n.process(b"\x1b[c");
    assert_eq!(resp, b"\x1b[?62;22c");
}

#[test]
fn test_da2_response() {
    let mut n = deny_negotiator();
    let resp = n.process(b"\x1b[>c");
    assert_eq!(resp, b"\x1b[>0;0;0c");
}

#[test]
fn test_decrpm_kitty_deny() {
    let mut n = deny_negotiator();
    let resp = n.process(b"\x1b[?2027$p");
    assert_eq!(resp, b"\x1b[?2027;2$y");
}

#[test]
fn test_cpr_response() {
    let mut n = deny_negotiator();
    let resp = n.process(b"\x1b[6n");
    assert_eq!(resp, b"\x1b[1;1R");
}

#[test]
fn test_cross_chunk_sequence() {
    let mut n = deny_negotiator();
    let r1 = n.process(b"\x1b[?");
    assert!(r1.is_empty());
    let r2 = n.process(b"u");
    assert_eq!(r2, b"\x1b[?0u");
}

#[test]
fn test_mixed_content_and_queries() {
    let mut n = deny_negotiator();
    let mut input = Vec::new();
    input.extend_from_slice(b"Hello ");
    input.extend_from_slice(b"\x1b[c");
    input.extend_from_slice(b" world ");
    input.extend_from_slice(b"\x1b[6n");
    let resp = n.process(&input);
    let mut expected = Vec::new();
    expected.extend_from_slice(b"\x1b[?62;22c");
    expected.extend_from_slice(b"\x1b[1;1R");
    assert_eq!(resp, expected);
}

#[test]
fn test_passthrough_no_responses() {
    let mut n = CapabilityNegotiator::new(CapabilityPolicy {
        keyboard: KeyboardPolicy::PassThrough,
        ..Default::default()
    });
    let resp = n.process(b"\x1b[?u\x1b[c\x1b[6n\x1b[?2027$p");
    assert!(resp.is_empty());
}
