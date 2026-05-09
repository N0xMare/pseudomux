use crate::error::{CoreError, Result};
use portable_pty::{PtyPair, PtySize, native_pty_system};

pub fn open(size: PtySize) -> Result<PtyPair> {
    let system = native_pty_system();
    let pair = system
        .openpty(size)
        .map_err(|e| CoreError::Msg(e.to_string()))?;
    Ok(pair)
}

pub fn resize(master: &mut dyn portable_pty::MasterPty, size: PtySize) -> Result<()> {
    master
        .resize(size)
        .map_err(|e| CoreError::Msg(e.to_string()))
}

/// Set the PTY to raw mode via the master fd.
/// This disables canonical mode, echo, and signal processing on the slave side,
/// allowing TUI applications to receive raw input bytes including escape sequences.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn set_raw_mode(master: &dyn portable_pty::MasterPty) -> Result<()> {
    if let Some(fd) = master.as_raw_fd() {
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &raw mut termios) != 0 {
                return Err(CoreError::Msg("tcgetattr failed".into()));
            }
            libc::cfmakeraw(&raw mut termios);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw const termios) != 0 {
                return Err(CoreError::Msg("tcsetattr failed".into()));
            }
        }
        Ok(())
    } else {
        // Not on Unix or no fd available — skip silently
        Ok(())
    }
}
