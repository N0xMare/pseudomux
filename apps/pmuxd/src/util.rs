use pseudomux_core::session::state::SessionInfo;
use pseudomux_protocol::{ContentEntryDto, SessionSummary};

pub(crate) fn summarize(info: SessionInfo) -> SessionSummary {
    SessionSummary {
        session: info.id,
        status: format!("{:?}", info.status),
        rows: info.size.rows,
        cols: info.size.cols,
        pid: info.pid,
        profile: info.launch.profile,
        agent: info.launch.agent,
        program: info.launch.program,
        args: info.launch.args,
        cwd: info.launch.cwd,
        name: info.name,
    }
}

pub(crate) fn entry_to_dto(e: &pseudomux_core::vte::ContentEntry) -> ContentEntryDto {
    ContentEntryDto {
        seq: e.seq,
        timestamp_ms: e
            .timestamp
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        tag: format!("{:?}", e.tag),
        text: e.text.clone(),
    }
}

pub(crate) fn parse_logging_mode(value: &str) -> pseudomux_core::session::state::LoggingMode {
    use pseudomux_core::session::state::LoggingMode;
    match value.to_ascii_lowercase().as_str() {
        "transcript" => LoggingMode::Transcript,
        "raw" | "raw+transcript" => LoggingMode::RawAndTranscript,
        _ => LoggingMode::Metadata,
    }
}
