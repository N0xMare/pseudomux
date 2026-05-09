pub mod profile;
pub mod response;
mod storage;
pub use storage::socket_candidates;

use pseudomux_adapters::input_profile::InputProfile;
use pseudomux_adapters::{AgentKind, LaunchConfig, to_start_spec};
use pseudomux_core::input::{KeyEvent, TerminalState};
use pseudomux_core::output::chunk::OutputChunk;
use pseudomux_core::session::StartSpec;
use pseudomux_core::session::manager::SessionManager;
use pseudomux_core::session::state::{SessionId, SessionInfo, TerminalSize};
use std::path::{Path, PathBuf};

/// Thin facade over [`SessionManager`] that adds:
/// - Log-root resolution (sessions_root)
/// - `start()` — converts AgentKind + LaunchConfig → StartSpec with log_dir_base set
/// - `resolve_profile()` / `send_action()` / `send_prompt()` — profile-aware input
/// - Error-type bridging: CoreError → anyhow::Error (via `?`)
///
/// The ~15 pass-through methods below are intentional boilerplate. Each one:
///
/// 1. Wraps the raw `SessionId` in a `SessionHandle` newtype (required by core API)
/// 2. Converts `CoreError` to `anyhow::Error` via `?`
///
/// A macro would shrink the line count but hurt IDE navigation and readability,
/// so we accept the repetition and document it here instead.
pub struct Service {
    core: SessionManager,
    log_root: PathBuf,
}

impl Service {
    pub fn new() -> anyhow::Result<Self> {
        let log_root = storage::sessions_root()?;
        Ok(Self {
            core: SessionManager::new(),
            log_root,
        })
    }

    /// Wrap a raw `SessionId` in the `SessionHandle` newtype expected by core.
    #[inline]
    fn h(id: SessionId) -> pseudomux_core::session::handle::SessionHandle {
        pseudomux_core::session::handle::SessionHandle(id)
    }

    pub fn start(
        &self,
        kind: AgentKind,
        cfg: LaunchConfig,
        name: Option<String>,
    ) -> anyhow::Result<SessionId> {
        let mut spec: StartSpec = to_start_spec(kind, cfg);
        spec.log_dir_base = Some(self.log_root.clone());
        spec.name = name;
        let handle = self.core.start_session(spec)?;
        Ok(handle.0)
    }

    pub fn log_root(&self) -> &Path {
        &self.log_root
    }

    // ── Pass-through methods ─────────────────────────────────────────────────
    // Each delegates to self.core with the SessionHandle wrapper and lets `?`
    // convert CoreError → anyhow::Error.

    pub fn send_text(&self, id: SessionId, text: &str) -> anyhow::Result<()> {
        Ok(self.core.send_text(Self::h(id), text)?)
    }
    pub fn send_bytes(&self, id: SessionId, bytes: &[u8]) -> anyhow::Result<()> {
        Ok(self.core.send_bytes(Self::h(id), bytes)?)
    }
    pub fn send_enter(&self, id: SessionId) -> anyhow::Result<()> {
        Ok(self.core.send_enter(Self::h(id))?)
    }
    pub fn read_since(&self, id: SessionId, seq: u64) -> anyhow::Result<(Vec<OutputChunk>, u64)> {
        Ok(self.core.read_since(Self::h(id), seq)?)
    }
    pub fn resize(&self, id: SessionId, size: TerminalSize) -> anyhow::Result<()> {
        Ok(self.core.resize(Self::h(id), size)?)
    }
    pub fn interrupt(&self, id: SessionId) -> anyhow::Result<()> {
        Ok(self.core.interrupt(Self::h(id))?)
    }
    pub fn terminate(&self, id: SessionId) -> anyhow::Result<()> {
        Ok(self.core.terminate(Self::h(id))?)
    }
    pub fn state(&self, id: SessionId) -> anyhow::Result<SessionInfo> {
        Ok(self.core.get_state(Self::h(id))?)
    }
    pub fn send_key(&self, id: SessionId, key: KeyEvent) -> anyhow::Result<()> {
        Ok(self.core.send_key(Self::h(id), key)?)
    }
    pub fn terminal_state(&self, id: SessionId) -> anyhow::Result<TerminalState> {
        Ok(self.core.terminal_state(Self::h(id))?)
    }
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        self.core.list_sessions()
    }
    pub fn subscribe_events(
        &self,
        id: SessionId,
    ) -> anyhow::Result<tokio::sync::broadcast::Receiver<pseudomux_core::vte::SemanticEvent>> {
        Ok(self.core.subscribe_events(Self::h(id))?)
    }
    pub fn agent_state(&self, id: SessionId) -> anyhow::Result<pseudomux_core::vte::AgentState> {
        Ok(self.core.agent_state(Self::h(id))?)
    }
    pub fn content_text(&self, id: SessionId) -> anyhow::Result<String> {
        Ok(self.core.content_text(Self::h(id))?)
    }
    pub fn status_text(&self, id: SessionId) -> anyhow::Result<String> {
        Ok(self.core.status_text(Self::h(id))?)
    }
    pub fn content_since_seq(
        &self,
        id: SessionId,
        seq: u64,
    ) -> anyhow::Result<Vec<pseudomux_core::vte::ContentEntry>> {
        Ok(self.core.content_since_seq(Self::h(id), seq)?)
    }
    pub fn content_since_last_input(
        &self,
        id: SessionId,
    ) -> anyhow::Result<Vec<pseudomux_core::vte::ContentEntry>> {
        Ok(self.core.content_since_last_input(Self::h(id))?)
    }
    pub fn content_text_since_last_input(&self, id: SessionId) -> anyhow::Result<String> {
        Ok(self.core.content_text_since_last_input(Self::h(id))?)
    }
    pub fn content_current_seq(&self, id: SessionId) -> anyhow::Result<u64> {
        Ok(self.core.content_current_seq(Self::h(id))?)
    }
    pub fn filtered_content_since_last_input(&self, id: SessionId) -> anyhow::Result<String> {
        Ok(self.core.filtered_content_since_last_input(Self::h(id))?)
    }
    pub fn filtered_content_since_seq(&self, id: SessionId, seq: u64) -> anyhow::Result<String> {
        Ok(self.core.filtered_content_since_seq(Self::h(id), seq)?)
    }
    pub fn filtered_screen_content(&self, id: SessionId) -> anyhow::Result<String> {
        Ok(self.core.filtered_screen_content(Self::h(id))?)
    }
    pub fn filtered_response_since_last_input(&self, id: SessionId) -> anyhow::Result<String> {
        Ok(self.core.filtered_response_since_last_input(Self::h(id))?)
    }
    pub fn subscribe_watch_events(
        &self,
        id: SessionId,
    ) -> anyhow::Result<tokio::sync::broadcast::Receiver<pseudomux_core::vte::WatchEvent>> {
        Ok(self.core.subscribe_watch_events(Self::h(id))?)
    }

    // ── Profile-aware input ──────────────────────────────────────────────────

    fn resolve_profile(&self, id: SessionId) -> anyhow::Result<(InputProfile, String)> {
        let handle = Self::h(id);
        let profile_name = self.core.input_profile_name(handle)?;
        let name = profile_name.as_deref().unwrap_or("shell");
        let profile = match name {
            "opencode" => InputProfile::opencode(),
            "claude_code" => InputProfile::claude_code(),
            "bubbletea_generic" => InputProfile::bubbletea_generic(),
            _ => InputProfile::shell(),
        };
        Ok((profile, name.to_string()))
    }

    fn send_action_keys(
        &self,
        id: SessionId,
        profile: &InputProfile,
        profile_name: &str,
        action: &str,
    ) -> anyhow::Result<()> {
        let handle = Self::h(id);
        let keys = profile.action_keys(action).ok_or_else(|| {
            anyhow::anyhow!("unknown action '{}' for profile '{}'", action, profile_name)
        })?;
        for key in keys {
            self.core.send_key(handle, *key)?;
        }
        Ok(())
    }

    pub fn send_action(&self, id: SessionId, action: &str) -> anyhow::Result<()> {
        let (profile, name) = self.resolve_profile(id)?;
        self.send_action_keys(id, &profile, &name, action)
    }

    pub fn send_prompt(&self, id: SessionId, text: &str) -> anyhow::Result<()> {
        let (profile, name) = self.resolve_profile(id)?;
        self.send_text(id, text)?;
        if profile.post_paste_delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(
                profile.post_paste_delay_ms,
            ));
        }
        self.send_action_keys(id, &profile, &name, "submit")?;
        Ok(())
    }
}
