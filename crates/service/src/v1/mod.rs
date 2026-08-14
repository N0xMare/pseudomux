//! Canonical v1 Claude session actors.
//!
//! This module owns high-level session/turn semantics and talks only to injected
//! terminal and transcript boundaries, which keeps correctness tests independent
//! of a real Claude process or rmux daemon.

mod actor;
mod backend;
mod minified;
mod registry;

pub(crate) use actor::require_tested_for_minified_cell;
pub use actor::{
    ClearRebind, SessionActorConfig, SessionActorHandle, StoredTurnTerminal,
    WritableAttachCompletion, is_valid_session_transition,
};
pub use backend::{
    Clock, DriverFailure, DriverResult, InterruptRecovery, POST_MARKER_CATCH_WINDOW_FLOOR_MS,
    SystemClock, TURN_DURATION_DRAIN_FLOOR_MS, TerminalControl, TerminalEvidence,
    TerminalScreenObservation, TranscriptArm, TranscriptBatch, TranscriptDrainEvidence,
    TranscriptPosition, TranscriptSource, UNRECOGNISED_SCREEN_VETO, graduated_drain_ms,
    post_marker_catch_window_ms,
};
pub use minified::{
    FastPathRefusal, FastPathVerdict, MINIFIED_FAST_PATH_DRAIN_FLOOR_MS, MinifiedTurnObservations,
    evaluate_minified_fast_path, minified_drain_ms,
};
/// Re-exported so the service and its embedders name exactly one cell type. The
/// definition lives in the protocol crate because the cell is now a wire field.
pub use pseudomux_protocol::v1::SessionCell;
pub use registry::{SessionOwner, SessionRegistration, SessionRegistry};
