use super::state::SessionId;

/// Lightweight handle to a live PTY session.
///
/// Copy-able reference used to address session operations on [`super::manager::SessionManager`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionHandle(pub SessionId);
