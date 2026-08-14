//! rmux-backed terminal control for pmux.
//!
//! This crate deliberately connects only to an explicit private rmux endpoint.
//! `pmuxd` creates that endpoint and starts its owned sidecar explicitly; no
//! runtime layer performs daemon discovery or implicit auto-start.

#[cfg(unix)]
mod attach;
mod backend;
mod launch;
mod process_boundary;

#[cfg(unix)]
pub use attach::{AttachCapabilityError, attach_capability_terminal, connect_attach_capability};

pub use backend::{
    BackendSessionRef, CellColor, ControlPlaneFault, RmuxBackend, RmuxBackendConfig, StyledCell,
    StyledScreen, TerminalBackend, TerminalBackendError, TerminalCursor, TerminalLaunch,
    TerminalSession, TerminalSnapshot, bracketed_paste_payload,
};
pub use launch::{
    EnvironmentSnapshot, LAUNCHER_PROTOCOL_VERSION, LaunchSpec, LaunchToken, LauncherRequest,
    LauncherResponse, MAX_LAUNCHER_FRAME_BYTES,
};
pub use process_boundary::{
    OwnedProcessBoundary, PROCESS_OBSERVATION_POLL_INTERVAL, ProcessBoundaryError,
    ProcessBoundaryObservation, try_reap_exited_child,
};

/// Exact private owner-pipe payload that distinguishes an orderly pmuxd
/// shutdown from kernel EOF caused by owner loss.
pub const OWNER_GRACEFUL_SHUTDOWN_FRAME: &[u8] = b"pmux-rmux-owner-graceful-v1\n";
