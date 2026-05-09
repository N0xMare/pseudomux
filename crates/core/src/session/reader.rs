//! Reader, wait, and quiescence threads for PTY sessions.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::SystemTime;

use serde_json::json;

use crate::input::CapabilityNegotiator;
use crate::session::logger::SessionLogger;
use crate::session::state::{SessionInfo, SessionStatus};
use crate::vte::{
    AgentState, ContentBuffer, ContentTag, RegionClassifier, ScreenChange, ScreenModel,
    SemanticEvent, WatchEvent, WatchEventBuilder,
};

/// Reader thread: reads PTY output, runs the VTE pipeline, and dispatches events.
///
/// This function is spawned as a [`std::thread::spawn`] background thread per session.
/// The many arguments reflect the set of shared-state handles the reader needs;
/// they are all `Arc`-wrapped and cheaply cloned at spawn time.
#[allow(clippy::too_many_arguments)] // by design: all args are required shared handles
pub(crate) fn run_reader(
    mut reader: Box<dyn Read + Send>,
    negotiator: Arc<Mutex<CapabilityNegotiator>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    scrollback: Arc<Mutex<crate::session::buffer::Scrollback>>,
    screen_model: Arc<Mutex<ScreenModel>>,
    classifier: Arc<Mutex<RegionClassifier>>,
    event_tx: tokio::sync::broadcast::Sender<SemanticEvent>,
    content_buffer: Arc<Mutex<ContentBuffer>>,
    watch_event_tx: tokio::sync::broadcast::Sender<WatchEvent>,
    watch_builder: Arc<Mutex<WatchEventBuilder>>,
    info: Arc<Mutex<SessionInfo>>,
    logger: Option<Arc<SessionLogger>>,
    alive: Arc<AtomicBool>,
    recorder: Option<std::sync::Mutex<std::io::BufWriter<std::fs::File>>>,
) {
    let mut buf = vec![0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if n > 0 {
                    // Capability negotiation: intercept escape sequences and respond
                    let responses = if let Ok(mut neg) = negotiator.lock() {
                        neg.process(&buf[..n])
                    } else {
                        vec![]
                    };
                    if !responses.is_empty()
                        && let Ok(mut w) = writer.lock()
                    {
                        let _ = w.write_all(&responses);
                        let _ = w.flush();
                    }
                    // Record raw PTY bytes before any processing
                    if let Some(ref rec) = recorder
                        && let Ok(mut w) = rec.lock()
                    {
                        let _ = w.write_all(&buf[..n]);
                        let _ = w.flush();
                    }
                    if let Ok(mut sb) = scrollback.lock() {
                        let chunk = buf[..n].to_vec();
                        let _ = sb.append(chunk);
                    }
                    // VTE semantic plane
                    let screen_changes = if let Ok(mut sm) = screen_model.lock() {
                        sm.process(&buf[..n])
                    } else {
                        vec![]
                    };
                    let semantic_events = if screen_changes.is_empty() {
                        vec![]
                    } else if let Ok(mut cl) = classifier.lock() {
                        cl.classify(&screen_changes)
                    } else {
                        vec![]
                    };
                    for event in &semantic_events {
                        let _ = event_tx.send(event.clone());
                    }

                    // Content buffer: capture stripped text from content row changes.
                    // The row index is recorded so row-aware snapshot methods can
                    // collapse ink/React re-renders to their final per-row value.
                    for change in &screen_changes {
                        if let ScreenChange::ContentRowChanged { row, new, .. } = change
                            && !new.is_empty()
                        {
                            let tag = if let Ok(cl) = classifier.lock() {
                                match cl.state() {
                                    AgentState::Thinking => ContentTag::AssistantOutput,
                                    AgentState::ToolRunning => ContentTag::ToolOutput,
                                    _ => ContentTag::Unknown,
                                }
                            } else {
                                ContentTag::Unknown
                            };
                            if let Ok(mut cb) = content_buffer.lock() {
                                cb.append_with_row(new.clone(), tag, *row);
                            }
                        }
                    }

                    // Watch events: convert semantic events to pilot-friendly events
                    if !semantic_events.is_empty()
                        && let Ok(mut wb) = watch_builder.lock()
                    {
                        for event in &semantic_events {
                            let watch_events = wb.process(event);
                            for we in watch_events {
                                let _ = watch_event_tx.send(we);
                            }
                        }
                    }

                    if let Ok(mut i) = info.lock() {
                        i.last_activity = SystemTime::now();
                    }
                    if let Some(logger) = &logger {
                        logger.log("OutputChunk", &json!({ "len": n }));
                    }
                }
            }
        }
    }
    alive.store(false, Ordering::Relaxed);
    if let Ok(mut i) = info.lock()
        && i.status != SessionStatus::Exited
    {
        i.status = SessionStatus::Exited;
    }
    if let Some(logger) = &logger {
        logger.log("SessionExited", &json!({}));
    }
}

/// Wait thread: waits for child process to exit, fires SessionExited event.
pub(crate) fn run_wait_thread(
    child: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>>,
    info: Arc<Mutex<SessionInfo>>,
    logger: Option<Arc<SessionLogger>>,
    event_tx: tokio::sync::broadcast::Sender<SemanticEvent>,
) {
    let result = {
        let mut guard = child.lock().unwrap();
        guard.take().map(|mut child| child.wait())
    };
    match result {
        Some(Ok(status)) => {
            if let Ok(mut i) = info.lock() {
                i.status = SessionStatus::Exited;
            }
            let exit_code = Some(status.exit_code() as i32);
            event_tx
                .send(SemanticEvent::SessionExited { exit_code, seq: 0 })
                .ok();
            if let Some(logger) = logger.as_ref() {
                logger.log(
                    "SessionExitStatus",
                    &json!({
                        "code": status.exit_code(),
                        "signal": status.signal(),
                        "success": status.success(),
                    }),
                );
            }
        }
        Some(Err(err)) => {
            event_tx
                .send(SemanticEvent::SessionExited {
                    exit_code: None,
                    seq: 0,
                })
                .ok();
            if let Some(logger) = logger.as_ref() {
                logger.log("SessionExitError", &json!({ "error": err.to_string() }));
            }
        }
        None => {}
    }
}

/// Quiescence thread: polls classifier for quiescence and fires events.
pub(crate) fn run_quiescence_thread(
    alive: Arc<AtomicBool>,
    classifier: Arc<Mutex<RegionClassifier>>,
    event_tx: tokio::sync::broadcast::Sender<SemanticEvent>,
) {
    let interval = Duration::from_millis(100);
    let threshold = Duration::from_millis(500);
    while alive.load(Ordering::Relaxed) {
        std::thread::sleep(interval);
        if let Ok(mut cl) = classifier.lock()
            && let Some(event) = cl.check_quiescence(threshold)
        {
            let _ = event_tx.send(event);
        }
    }
}
