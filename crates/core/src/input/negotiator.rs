/// Terminal capability negotiation for PTY output streams.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KittyKeyboardMode {
    Legacy,
    Kitty(u8),
}

#[derive(Clone, Debug)]
pub struct TerminalState {
    pub keyboard_mode: KittyKeyboardMode,
    keyboard_mode_stack: Vec<u8>,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub sync_output: bool,
}

impl Default for TerminalState {
    fn default() -> Self {
        Self {
            keyboard_mode: KittyKeyboardMode::Legacy,
            keyboard_mode_stack: Vec::new(),
            bracketed_paste: false,
            focus_events: false,
            sync_output: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardPolicy {
    Deny,
    Accept,
    PassThrough,
}

#[derive(Clone, Debug)]
pub struct CapabilityPolicy {
    pub keyboard: KeyboardPolicy,
    pub term_env: String,
    pub colorterm_env: String,
}

impl Default for CapabilityPolicy {
    fn default() -> Self {
        Self {
            keyboard: KeyboardPolicy::Deny,
            term_env: "xterm-256color".to_string(),
            colorterm_env: "truecolor".to_string(),
        }
    }
}

pub struct CapabilityNegotiator {
    state: TerminalState,
    policy: CapabilityPolicy,
    esc_buf: Vec<u8>,
    in_escape: bool,
}

impl CapabilityNegotiator {
    pub fn new(policy: CapabilityPolicy) -> Self {
        Self {
            state: TerminalState::default(),
            policy,
            esc_buf: Vec::new(),
            in_escape: false,
        }
    }

    pub fn state(&self) -> &TerminalState {
        &self.state
    }

    pub fn process(&mut self, output: &[u8]) -> Vec<u8> {
        if self.policy.keyboard == KeyboardPolicy::PassThrough {
            return Vec::new();
        }

        let mut responses = Vec::new();

        for &byte in output {
            if byte == 0x1b {
                self.esc_buf.clear();
                self.esc_buf.push(byte);
                self.in_escape = true;
                continue;
            }

            if !self.in_escape {
                continue;
            }

            self.esc_buf.push(byte);

            if self.esc_buf.len() > 32 {
                self.esc_buf.clear();
                self.in_escape = false;
                continue;
            }

            if let Some(resp) = self.try_complete_sequence() {
                responses.extend_from_slice(&resp);
                self.esc_buf.clear();
                self.in_escape = false;
            }
        }

        responses
    }

    fn try_complete_sequence(&mut self) -> Option<Vec<u8>> {
        let buf = &self.esc_buf;
        let len = buf.len();

        if len < 2 {
            return None;
        }

        // Only handle CSI sequences (ESC [)
        if buf[1] != b'[' {
            // Non-CSI: abandon after ESC + one byte
            return Some(Vec::new());
        }

        // Need at least ESC [ <something> to check termination
        if len < 3 {
            return None;
        }

        let last = buf[len - 1];

        // Special case: $ introduces a two-char final sequence ($p, $y)
        if last == b'$' {
            // Wait for next byte
            return None;
        }
        // If previous byte was $, this byte terminates
        if len >= 4 && buf[len - 2] == b'$' && (0x40..=0x7e).contains(&last) {
            return Some(self.handle_csi_sequence());
        }

        // Normal CSI termination: final byte in 0x40-0x7E
        if (0x40..=0x7e).contains(&last) {
            return Some(self.handle_csi_sequence());
        }

        None
    }

    fn handle_csi_sequence(&mut self) -> Vec<u8> {
        let seq = String::from_utf8_lossy(&self.esc_buf).to_string();

        // DA1: ESC [ c
        if seq == "\x1b[c" {
            return b"\x1b[?62;22c".to_vec();
        }
        // DA2: ESC [ > c
        if seq == "\x1b[>c" {
            return b"\x1b[>0;0;0c".to_vec();
        }
        // CPR: ESC [ 6 n
        if seq == "\x1b[6n" {
            return b"\x1b[1;1R".to_vec();
        }
        // Pixel size: ESC [ 14 t
        if seq == "\x1b[14t" {
            return b"\x1b[4;384;640t".to_vec();
        }
        // Kitty keyboard query: ESC [ ? u
        if seq == "\x1b[?u" {
            return match self.policy.keyboard {
                KeyboardPolicy::Deny => b"\x1b[?0u".to_vec(),
                KeyboardPolicy::Accept => {
                    let mode = match self.state.keyboard_mode {
                        KittyKeyboardMode::Legacy => 0,
                        KittyKeyboardMode::Kitty(m) => m,
                    };
                    format!("\x1b[?{mode}u").into_bytes()
                }
                KeyboardPolicy::PassThrough => Vec::new(),
            };
        }
        // Kitty keyboard pop: ESC [ < u
        if seq == "\x1b[<u" {
            if self.policy.keyboard == KeyboardPolicy::Accept {
                if let Some(mode) = self.state.keyboard_mode_stack.pop() {
                    self.state.keyboard_mode = if mode == 0 {
                        KittyKeyboardMode::Legacy
                    } else {
                        KittyKeyboardMode::Kitty(mode)
                    };
                } else {
                    self.state.keyboard_mode = KittyKeyboardMode::Legacy;
                }
            }
            return Vec::new();
        }
        // xterm modifyOtherKeys: ESC [ > 4 ; N m
        // This is NOT Kitty keyboard push. Do not modify keyboard_mode.
        if seq.starts_with("\x1b[>4;") && seq.ends_with('m') {
            return Vec::new();
        }
        // Kitty keyboard push: ESC [ > N u (the ACTUAL Kitty push command)
        // We always track the push regardless of policy. BubbleTea (and similar TUIs)
        // ignore Deny responses and parse Kitty input anyway, so tracking state under
        // Deny ensures we produce correct key encodings for the actual TUI behavior.
        if seq.starts_with("\x1b[>") && seq.ends_with('u') {
            let inner = &seq[3..seq.len() - 1];
            if let Ok(flags) = inner.parse::<u8>() {
                let current = match self.state.keyboard_mode {
                    KittyKeyboardMode::Legacy => 0,
                    KittyKeyboardMode::Kitty(m) => m,
                };
                self.state.keyboard_mode_stack.push(current);
                self.state.keyboard_mode = if flags == 0 {
                    KittyKeyboardMode::Legacy
                } else {
                    KittyKeyboardMode::Kitty(flags)
                };
            }
            return Vec::new();
        }
        // Bracketed paste
        if seq == "\x1b[?2004h" {
            self.state.bracketed_paste = true;
            return Vec::new();
        }
        if seq == "\x1b[?2004l" {
            self.state.bracketed_paste = false;
            return Vec::new();
        }
        // Focus events
        if seq == "\x1b[?1004h" {
            self.state.focus_events = true;
            return Vec::new();
        }
        if seq == "\x1b[?1004l" {
            self.state.focus_events = false;
            return Vec::new();
        }
        // Sync output
        if seq == "\x1b[?2026h" {
            self.state.sync_output = true;
            return Vec::new();
        }
        if seq == "\x1b[?2026l" {
            self.state.sync_output = false;
            return Vec::new();
        }
        // DECRPM Kitty query: ESC [ ? 2027 $ p
        if seq == "\x1b[?2027$p" {
            return match self.policy.keyboard {
                KeyboardPolicy::Deny => b"\x1b[?2027;2$y".to_vec(),
                KeyboardPolicy::Accept => b"\x1b[?2027;1$y".to_vec(),
                KeyboardPolicy::PassThrough => Vec::new(),
            };
        }
        // DEC Kitty enable
        if seq == "\x1b[?2027h" {
            if self.policy.keyboard == KeyboardPolicy::Accept {
                self.state.keyboard_mode = KittyKeyboardMode::Kitty(1);
            }
            return Vec::new();
        }

        Vec::new()
    }
}
