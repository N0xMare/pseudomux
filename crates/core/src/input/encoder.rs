//! Key encoding for terminal input.
//!
//! Converts high-level key events into the correct byte sequences
//! based on the negotiated terminal capabilities (legacy vs Kitty protocol).
use super::negotiator::{KittyKeyboardMode, TerminalState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    Char(char),
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Function(u8),
    Ctrl(char),
    Alt(char),
    CtrlAlt(char),
}

fn ctrl_byte(c: char) -> u8 {
    match c {
        '@' => 0,
        'a'..='z' => c as u8 - 0x60,
        'A'..='Z' => c as u8 - 0x40,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        _ => c as u8, // fallback
    }
}

pub fn encode_key(key: KeyEvent, state: &TerminalState) -> Vec<u8> {
    match state.keyboard_mode {
        KittyKeyboardMode::Legacy => encode_key_legacy(key),
        KittyKeyboardMode::Kitty(_) => encode_key_kitty(key),
    }
}

fn encode_key_legacy(key: KeyEvent) -> Vec<u8> {
    match key {
        KeyEvent::Enter => vec![0x0d],
        KeyEvent::Tab => vec![0x09],
        KeyEvent::Escape => vec![0x1b],
        KeyEvent::Backspace => vec![0x7f],
        KeyEvent::Delete => b"\x1b[3~".to_vec(),
        KeyEvent::Up => b"\x1b[A".to_vec(),
        KeyEvent::Down => b"\x1b[B".to_vec(),
        KeyEvent::Right => b"\x1b[C".to_vec(),
        KeyEvent::Left => b"\x1b[D".to_vec(),
        KeyEvent::Home => b"\x1b[H".to_vec(),
        KeyEvent::End => b"\x1b[F".to_vec(),
        KeyEvent::PageUp => b"\x1b[5~".to_vec(),
        KeyEvent::PageDown => b"\x1b[6~".to_vec(),
        KeyEvent::Function(n) => encode_function_key(n),
        KeyEvent::Ctrl(c) => vec![ctrl_byte(c)],
        KeyEvent::Alt(c) => {
            let mut v = vec![0x1b];
            let mut buf = [0u8; 4];
            v.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            v
        }
        KeyEvent::CtrlAlt(c) => {
            vec![0x1b, ctrl_byte(c)]
        }
        KeyEvent::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            s.as_bytes().to_vec()
        }
    }
}

fn encode_function_key(n: u8) -> Vec<u8> {
    match n {
        1 => b"\x1bOP".to_vec(),
        2 => b"\x1bOQ".to_vec(),
        3 => b"\x1bOR".to_vec(),
        4 => b"\x1bOS".to_vec(),
        5 => b"\x1b[15~".to_vec(),
        6 => b"\x1b[17~".to_vec(),
        7 => b"\x1b[18~".to_vec(),
        8 => b"\x1b[19~".to_vec(),
        9 => b"\x1b[20~".to_vec(),
        10 => b"\x1b[21~".to_vec(),
        11 => b"\x1b[23~".to_vec(),
        12 => b"\x1b[24~".to_vec(),
        _ => Vec::new(),
    }
}

fn encode_key_kitty(key: KeyEvent) -> Vec<u8> {
    match key {
        KeyEvent::Enter => b"\x1b[13u".to_vec(),
        KeyEvent::Tab => b"\x1b[9u".to_vec(),
        KeyEvent::Escape => b"\x1b[27u".to_vec(),
        KeyEvent::Backspace => b"\x1b[127u".to_vec(),
        KeyEvent::Delete => b"\x1b[3~".to_vec(),
        KeyEvent::Up => b"\x1b[A".to_vec(),
        KeyEvent::Down => b"\x1b[B".to_vec(),
        KeyEvent::Right => b"\x1b[C".to_vec(),
        KeyEvent::Left => b"\x1b[D".to_vec(),
        KeyEvent::Home => b"\x1b[H".to_vec(),
        KeyEvent::End => b"\x1b[F".to_vec(),
        KeyEvent::PageUp => b"\x1b[5~".to_vec(),
        KeyEvent::PageDown => b"\x1b[6~".to_vec(),
        KeyEvent::Function(n) => encode_function_key(n),
        KeyEvent::Ctrl(c) => format!("\x1b[{};5u", c as u32).into_bytes(),
        KeyEvent::Alt(c) => format!("\x1b[{};3u", c as u32).into_bytes(),
        KeyEvent::CtrlAlt(c) => format!("\x1b[{};7u", c as u32).into_bytes(),
        KeyEvent::Char(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            s.as_bytes().to_vec()
        }
    }
}

pub fn encode_text(text: &str, state: &TerminalState) -> Vec<u8> {
    if state.bracketed_paste {
        let mut out = Vec::new();
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.as_bytes().to_vec()
    }
}

pub fn parse_key_name(name: &str) -> Result<KeyEvent, String> {
    let lower = name.to_lowercase();

    // Check for modifiers
    if let Some(rest) = lower.strip_prefix("ctrl+alt+") {
        let ch = parse_single_char(rest)?;
        return Ok(KeyEvent::CtrlAlt(ch));
    }
    if let Some(rest) = lower.strip_prefix("ctrl+") {
        let ch = parse_single_char(rest)?;
        return Ok(KeyEvent::Ctrl(ch));
    }
    if let Some(rest) = lower.strip_prefix("alt+") {
        let ch = parse_single_char(rest)?;
        return Ok(KeyEvent::Alt(ch));
    }

    match lower.as_str() {
        "enter" | "return" => Ok(KeyEvent::Enter),
        "tab" => Ok(KeyEvent::Tab),
        "escape" | "esc" => Ok(KeyEvent::Escape),
        "backspace" => Ok(KeyEvent::Backspace),
        "delete" | "del" => Ok(KeyEvent::Delete),
        "up" => Ok(KeyEvent::Up),
        "down" => Ok(KeyEvent::Down),
        "left" => Ok(KeyEvent::Left),
        "right" => Ok(KeyEvent::Right),
        "home" => Ok(KeyEvent::Home),
        "end" => Ok(KeyEvent::End),
        "pageup" | "pgup" => Ok(KeyEvent::PageUp),
        "pagedown" | "pgdn" => Ok(KeyEvent::PageDown),
        s if s.starts_with('f') => {
            let num = s[1..]
                .parse::<u8>()
                .map_err(|_| format!("invalid function key: {name}"))?;
            if (1..=12).contains(&num) {
                Ok(KeyEvent::Function(num))
            } else {
                Err(format!("function key out of range: {name}"))
            }
        }
        s if s.chars().count() == 1 => Ok(KeyEvent::Char(s.chars().next().unwrap())),
        _ => Err(format!("unknown key name: {name}")),
    }
}

fn parse_single_char(s: &str) -> Result<char, String> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(format!("expected single character, got: {s}")),
    }
}
