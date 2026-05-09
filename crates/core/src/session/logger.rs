use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::error::{CoreError, Result};
use crate::session::state::{LoggingMode, SessionId};

pub(crate) fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub(crate) struct SessionLogger {
    pub(crate) mode: LoggingMode,
    pub(crate) file: Mutex<Option<std::fs::File>>,
}

impl SessionLogger {
    pub(crate) fn create(base: &Path, id: SessionId, mode: LoggingMode) -> Result<Self> {
        let dir = base.join(id.to_string());
        std::fs::create_dir_all(&dir).map_err(|e| CoreError::Msg(e.to_string()))?;
        let file_path = dir.join("events.ndjson");
        let mut opts = OpenOptions::new();
        opts.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts
            .open(&file_path)
            .map_err(|e| CoreError::Msg(e.to_string()))?;
        Ok(Self {
            mode,
            file: Mutex::new(Some(file)),
        })
    }

    pub(crate) fn log(&self, kind: &str, data: &serde_json::Value) {
        if !matches!(
            self.mode,
            LoggingMode::Metadata | LoggingMode::Transcript | LoggingMode::RawAndTranscript
        ) {
            return;
        }
        if let Ok(mut guard) = self.file.lock()
            && let Some(file) = guard.as_mut()
        {
            let record = json!({
                "ts_ms": now_millis(),
                "kind": kind,
                "data": data,
            });
            if let Ok(line) = serde_json::to_string(&record) {
                let _ = writeln!(file, "{line}");
            }
        }
    }
}
