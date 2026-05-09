use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionStatus {
    Starting,
    Ready,
    Idle,
    Busy,
    Exited,
    Hung,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum LoggingMode {
    Metadata,
    Transcript,
    RawAndTranscript,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScrollbackConfig {
    pub raw_bytes: usize,
    pub stripped_bytes: usize,
}

impl Default for ScrollbackConfig {
    fn default() -> Self {
        Self {
            raw_bytes: 8 * 1024 * 1024,
            stripped_bytes: 4 * 1024 * 1024,
        }
    }
}

pub type SessionId = Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionLaunchMeta {
    pub profile: Option<String>,
    pub agent: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub pid: Option<u32>,
    pub size: TerminalSize,
    pub status: SessionStatus,
    pub created_at: SystemTime,
    pub last_activity: SystemTime,
    pub launch: SessionLaunchMeta,
    /// Human-readable session name for identification.
    pub name: Option<String>,
}

use crate::adapter::TuiAdapter;
use std::path::PathBuf;
use std::sync::Arc;

pub struct StartSpec {
    pub profile: Option<String>,
    pub agent: String,
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    pub size: TerminalSize,
    pub scrollback: ScrollbackConfig,
    pub logging: LoggingMode,
    pub log_dir_base: Option<PathBuf>,
    pub agent_kind: Option<String>,
    pub capability_policy_keyboard: Option<String>,
    pub input_profile_name: Option<String>,
    pub env_remove: Vec<String>,
    pub adapter: Option<Arc<dyn TuiAdapter>>,
    pub record_path: Option<PathBuf>,
    pub name: Option<String>,
}
