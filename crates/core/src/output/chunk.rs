use serde::{Deserialize, Serialize};

/// A raw PTY output chunk with a monotonically increasing sequence number.
///
/// Stored in [`crate::session::buffer::Scrollback`] and streamed to callers via
/// [`crate::session::manager::SessionManager::read_since`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputChunk {
    /// Monotonically increasing sequence number (1-based).
    pub seq: u64,
    /// Raw bytes as received from the PTY master.
    pub bytes: Vec<u8>,
}
