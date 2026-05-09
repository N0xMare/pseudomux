use std::collections::HashMap;

/// Keyboard protocol policy — how pseudomux responds to Kitty keyboard queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardPolicy {
    /// Respond "not supported" to Kitty queries. TUI falls back to legacy input.
    Deny,
    /// Respond affirmatively. Requires Kitty CSI u encoding for all keys.
    Accept,
    /// Don't respond to queries (broken behavior, testing only).
    PassThrough,
}

/// Policy for terminal capability negotiation.
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

pub use pseudomux_core::input::KeyEvent;

/// Agent-specific input configuration.
#[derive(Clone, Debug)]
pub struct InputProfile {
    /// Capability policy for this agent type.
    pub capability_policy: CapabilityPolicy,
    /// Named actions → key sequences. e.g., "submit" → [Enter]
    pub actions: HashMap<String, Vec<KeyEvent>>,
    /// Fallback actions if primary fails. e.g., "submit" fallback → [Ctrl('s')]
    pub fallback_actions: HashMap<String, Vec<KeyEvent>>,
    /// Delay in ms between sending pasted text and the submit key.
    /// Needed for TUIs (like ink/React) that process paste events asynchronously.
    pub post_paste_delay_ms: u64,
    /// Env vars to inject into the PTY.
    pub env: Vec<(String, String)>,
}

impl InputProfile {
    pub fn shell() -> Self {
        Self {
            capability_policy: CapabilityPolicy::default(),
            actions: HashMap::from([
                ("submit".into(), vec![KeyEvent::Enter]),
                ("interrupt".into(), vec![KeyEvent::Ctrl('c')]),
                ("eof".into(), vec![KeyEvent::Ctrl('d')]),
            ]),
            fallback_actions: HashMap::new(),
            post_paste_delay_ms: 0,
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ],
        }
    }

    pub fn opencode() -> Self {
        Self {
            capability_policy: CapabilityPolicy {
                keyboard: KeyboardPolicy::Deny,
                ..Default::default()
            },
            actions: HashMap::from([
                ("submit".into(), vec![KeyEvent::Enter]),
                ("interrupt".into(), vec![KeyEvent::Escape]),
                ("command_palette".into(), vec![KeyEvent::Ctrl('p')]),
                ("variants".into(), vec![KeyEvent::Ctrl('t')]),
                ("agents".into(), vec![KeyEvent::Tab]),
                ("confirm_yes".into(), vec![KeyEvent::Char('y')]),
                ("confirm_no".into(), vec![KeyEvent::Char('n')]),
            ]),
            fallback_actions: HashMap::from([("submit".into(), vec![KeyEvent::Ctrl('s')])]),
            post_paste_delay_ms: 0,
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ],
        }
    }

    pub fn claude_code() -> Self {
        Self {
            capability_policy: CapabilityPolicy {
                keyboard: KeyboardPolicy::Deny,
                ..Default::default()
            },
            actions: HashMap::from([
                ("submit".into(), vec![KeyEvent::Enter]),
                ("interrupt".into(), vec![KeyEvent::Escape]),
                ("hard_interrupt".into(), vec![KeyEvent::Ctrl('c')]),
                ("confirm_yes".into(), vec![KeyEvent::Char('y')]),
                ("confirm_no".into(), vec![KeyEvent::Char('n')]),
            ]),
            fallback_actions: HashMap::new(),
            post_paste_delay_ms: 500,
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ],
        }
    }

    pub fn bubbletea_generic() -> Self {
        Self {
            capability_policy: CapabilityPolicy {
                keyboard: KeyboardPolicy::Deny,
                ..Default::default()
            },
            actions: HashMap::from([
                ("submit".into(), vec![KeyEvent::Enter]),
                ("interrupt".into(), vec![KeyEvent::Ctrl('c')]),
            ]),
            fallback_actions: HashMap::new(),
            post_paste_delay_ms: 0,
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ],
        }
    }

    /// Look up an action's key sequence.
    pub fn action_keys(&self, action: &str) -> Option<&Vec<KeyEvent>> {
        self.actions.get(action)
    }

    /// Look up a fallback action's key sequence.
    pub fn fallback_keys(&self, action: &str) -> Option<&Vec<KeyEvent>> {
        self.fallback_actions.get(action)
    }
}
