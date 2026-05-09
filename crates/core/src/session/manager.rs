use super::handle::SessionHandle;
use super::state::{
    SessionId, SessionInfo, SessionLaunchMeta, SessionStatus, StartSpec, TerminalSize,
};
use crate::error::{CoreError, Result};
use crate::output::chunk::OutputChunk;
use crate::pty::backend;
use crate::session::buffer::Scrollback;
use crate::session::logger::SessionLogger;
use crate::session::reader;
use portable_pty::{ChildKiller, CommandBuilder, PtySize};
use serde_json::json;
use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tracing::warn;
use uuid::Uuid;

use crate::adapter::{ContentFilterTrait, TuiAdapter};
use crate::input::{
    CapabilityNegotiator, CapabilityPolicy, KeyEvent, KeyboardPolicy, TerminalState, encode_key,
    encode_text,
};
use crate::vte::{
    AgentState, ContentBuffer, ContentEntry, RegionClassifier, ScreenModel, ScreenRegions,
    SemanticEvent, StatusPatterns, WatchEvent, WatchEventBuilder,
};
use std::sync::atomic::AtomicBool;

struct Session {
    info: Arc<Mutex<SessionInfo>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    negotiator: Arc<Mutex<CapabilityNegotiator>>,
    _child: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    scrollback: Arc<Mutex<Scrollback>>,
    logger: Option<Arc<SessionLogger>>,
    screen_model: Arc<Mutex<ScreenModel>>,
    classifier: Arc<Mutex<RegionClassifier>>,
    event_tx: tokio::sync::broadcast::Sender<SemanticEvent>,
    content_buffer: Arc<Mutex<ContentBuffer>>,
    watch_event_tx: tokio::sync::broadcast::Sender<WatchEvent>,
    adapter: Option<Arc<dyn TuiAdapter>>,
    input_profile_name: Option<String>,
    _session_alive: Arc<AtomicBool>,
    _reader_thread: std::thread::JoinHandle<()>,
    _wait_thread: std::thread::JoinHandle<()>,
    _quiescence_thread: std::thread::JoinHandle<()>,
}

/// Manages the lifecycle of all PTY sessions in-process.
///
/// Each session owns a PTY pair, a VTE screen model, a classifier, a scrollback
/// ring-buffer, and three background threads (reader, wait, quiescence).
/// Operations are synchronous and thread-safe via internal mutexes.
pub struct SessionManager {
    sessions: Mutex<HashMap<SessionId, Arc<Session>>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Launch a new PTY session from a [`StartSpec`] and return a [`SessionHandle`].
    pub fn start_session(&self, spec: StartSpec) -> Result<SessionHandle> {
        let StartSpec {
            profile,
            agent,
            program,
            args,
            env,
            cwd,
            size,
            scrollback,
            logging,
            log_dir_base,
            agent_kind,
            capability_policy_keyboard,
            input_profile_name,
            env_remove,
            adapter,
            record_path,
            name: _,
        } = spec;

        let id = Uuid::new_v4();
        let pty_size = PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = backend::open(pty_size)?;

        #[cfg(unix)]
        {
            if let Err(e) = backend::set_raw_mode(pair.master.as_ref()) {
                tracing::warn!("failed to set PTY raw mode: {e:?}");
            }
        }

        let mut cmd = CommandBuilder::new(&program);
        for a in &args {
            cmd.arg(a);
        }
        for (k, v) in &env {
            cmd.env(k, v);
        }
        for k in &env_remove {
            cmd.env_remove(k);
        }
        if let Some(cwd) = &cwd {
            cmd.cwd(cwd);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| CoreError::Msg(e.to_string()))?;
        let killer = child.clone_killer();

        let pty_reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| CoreError::Msg(e.to_string()))?;
        let pty_writer = pair
            .master
            .take_writer()
            .map_err(|e| CoreError::Msg(e.to_string()))?;

        let created_at = SystemTime::now();
        let pid_opt = child.process_id();
        let launch = SessionLaunchMeta {
            profile: profile.clone(),
            agent: agent.clone(),
            program: program.clone(),
            args: args.clone(),
            cwd: cwd.as_ref().map(|p| p.display().to_string()),
        };
        let info = SessionInfo {
            id,
            pid: pid_opt,
            size,
            status: SessionStatus::Ready,
            created_at,
            last_activity: created_at,
            launch,
            name: spec.name.clone(),
        };

        let scroll_cap = scrollback.raw_bytes;
        let scrollback = Arc::new(Mutex::new(Scrollback::new(scroll_cap)));
        let info_mutex = Arc::new(Mutex::new(info));
        let master_mutex: Mutex<Box<dyn portable_pty::MasterPty + Send>> = Mutex::new(pair.master);
        let child_mutex: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send>>>> =
            Arc::new(Mutex::new(Some(child)));
        let killer_mutex: Mutex<Box<dyn ChildKiller + Send + Sync>> = Mutex::new(killer);
        let capability_policy = match capability_policy_keyboard.as_deref() {
            Some("accept") => CapabilityPolicy {
                keyboard: KeyboardPolicy::Accept,
                ..Default::default()
            },
            Some("passthrough") => CapabilityPolicy {
                keyboard: KeyboardPolicy::PassThrough,
                ..Default::default()
            },
            _ => CapabilityPolicy::default(),
        };
        let negotiator = Arc::new(Mutex::new(CapabilityNegotiator::new(capability_policy)));
        let writer_mutex: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(pty_writer));

        let logger =
            log_dir_base
                .as_ref()
                .and_then(|base| match SessionLogger::create(base, id, logging) {
                    Ok(l) => Some(Arc::new(l)),
                    Err(err) => {
                        warn!("failed to init session logger: {err:?}");
                        None
                    }
                });

        // Use adapter screen regions if provided; otherwise fall back to agent_kind string
        // (the legacy non-adapter path still supported for backward-compat).
        let regions = if let Some(ref adapter) = adapter {
            adapter.screen_regions(size.rows, size.cols)
        } else {
            match agent_kind.as_deref() {
                Some("opencode") => ScreenRegions::opencode(size.rows, size.cols),
                Some("claude-code") => ScreenRegions::claude_code(size.rows, size.cols),
                _ => ScreenRegions::full_screen(size.rows, size.cols),
            }
        };
        let screen_model = Arc::new(Mutex::new(ScreenModel::new(
            size.rows, size.cols, 1000, regions,
        )));
        let patterns = if let Some(ref adapter) = adapter {
            adapter.status_patterns()
        } else {
            match agent_kind.as_deref() {
                Some("opencode") => StatusPatterns::opencode_v1_2(),
                Some("claude-code") => StatusPatterns::claude_code(),
                _ => StatusPatterns::default(),
            }
        };
        let classifier = {
            let c = RegionClassifier::new(patterns);
            if let Some(ref adapter) = adapter {
                let adapter_clone = Arc::clone(adapter);
                Arc::new(Mutex::new(c.with_confirmation_checker(Arc::new(
                    move |text: &str| adapter_clone.is_confirmation(text),
                ))))
            } else {
                Arc::new(Mutex::new(c))
            }
        };
        let (event_tx, _) = tokio::sync::broadcast::channel::<SemanticEvent>(256);
        let content_buffer = Arc::new(Mutex::new(ContentBuffer::default()));
        let (watch_event_tx, _) = tokio::sync::broadcast::channel::<WatchEvent>(256);
        let watch_builder = Arc::new(Mutex::new(WatchEventBuilder::new()));
        let session_alive = Arc::new(AtomicBool::new(true));

        // Create PTY recorder if record_path is set
        let recorder = record_path.as_ref().and_then(|path| {
            match std::fs::File::create(path) {
                Ok(file) => {
                    // Write companion metadata file
                    let meta_path = path.with_extension("meta.json");
                    let meta = json!({
                        "agent": &agent,
                        "profile": &profile,
                        "rows": size.rows,
                        "cols": size.cols,
                        "timestamp": SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
                    });
                    if let Ok(meta_file) = std::fs::File::create(&meta_path) {
                        let _ = serde_json::to_writer_pretty(meta_file, &meta);
                    }
                    Some(std::sync::Mutex::new(std::io::BufWriter::new(file)))
                }
                Err(e) => {
                    warn!("failed to create recording file {}: {e}", path.display());
                    None
                }
            }
        });

        // Spawn reader thread
        let reader_thread = {
            let negotiator = Arc::clone(&negotiator);
            let writer = Arc::clone(&writer_mutex);
            let scrollback = Arc::clone(&scrollback);
            let screen_model = Arc::clone(&screen_model);
            let classifier = Arc::clone(&classifier);
            let event_tx = event_tx.clone();
            let content_buffer = Arc::clone(&content_buffer);
            let watch_event_tx = watch_event_tx.clone();
            let watch_builder = Arc::clone(&watch_builder);
            let info = Arc::clone(&info_mutex);
            let logger = logger.as_ref().map(Arc::clone);
            let alive = Arc::clone(&session_alive);
            std::thread::spawn(move || {
                reader::run_reader(
                    pty_reader,
                    negotiator,
                    writer,
                    scrollback,
                    screen_model,
                    classifier,
                    event_tx,
                    content_buffer,
                    watch_event_tx,
                    watch_builder,
                    info,
                    logger,
                    alive,
                    recorder,
                )
            })
        };

        // Spawn wait thread
        let wait_thread = {
            let child = Arc::clone(&child_mutex);
            let info = Arc::clone(&info_mutex);
            let logger = logger.clone();
            let event_tx = event_tx.clone();
            std::thread::spawn(move || reader::run_wait_thread(child, info, logger, event_tx))
        };

        // Spawn quiescence thread
        let quiescence_thread = {
            let alive = Arc::clone(&session_alive);
            let classifier = Arc::clone(&classifier);
            let event_tx = event_tx.clone();
            std::thread::spawn(move || reader::run_quiescence_thread(alive, classifier, event_tx))
        };

        let session = Arc::new(Session {
            info: info_mutex,
            master: master_mutex,
            writer: writer_mutex,
            negotiator,
            _child: child_mutex,
            killer: killer_mutex,
            scrollback,
            logger: logger.clone(),
            screen_model,
            classifier,
            event_tx: event_tx.clone(),
            content_buffer,
            watch_event_tx,
            adapter: adapter.map(|a| a as Arc<dyn TuiAdapter>),
            input_profile_name: input_profile_name.clone(),
            _session_alive: session_alive,
            _reader_thread: reader_thread,
            _wait_thread: wait_thread,
            _quiescence_thread: quiescence_thread,
        });

        self.sessions.lock().unwrap().insert(id, session);

        if let Some(logger) = logger.as_ref() {
            logger.log(
                "SessionStarted",
                &json!({
                    "program": program,
                    "agent": agent,
                    "profile": profile,
                    "args": args,
                    "cwd": cwd.as_ref().map(|p| p.display().to_string()),
                    "rows": size.rows,
                    "cols": size.cols,
                    "pid": pid_opt,
                }),
            );
            logger.log("SessionReady", &json!({}));
        }

        Ok(SessionHandle(id))
    }

    /// Write text to the PTY as if typed by the user.
    ///
    /// Text is encoded according to the negotiated terminal capabilities.
    pub fn send_text(&self, session: SessionHandle, text: &str) -> Result<()> {
        let s = self.get_session(session)?;
        let state = s.negotiator.lock().unwrap().state().clone();
        let bytes = encode_text(text, &state);
        let mut w = s.writer.lock().unwrap();
        w.write_all(&bytes)
            .map_err(|e| CoreError::Msg(e.to_string()))?;
        w.flush().ok();
        if let Ok(mut cb) = s.content_buffer.lock() {
            cb.mark_input_boundary();
        }
        if let Some(logger) = &s.logger {
            logger.log("InputSent", &json!({ "len": bytes.len(), "kind": "text" }));
        }
        Ok(())
    }

    /// Write raw bytes directly to the PTY (no encoding applied).
    pub fn send_bytes(&self, session: SessionHandle, bytes: &[u8]) -> Result<()> {
        let s = self.get_session(session)?;
        let mut w = s.writer.lock().unwrap();
        w.write_all(bytes)
            .map_err(|e| CoreError::Msg(e.to_string()))?;
        w.flush().ok();
        if let Some(logger) = &s.logger {
            logger.log("InputSent", &json!({ "len": bytes.len(), "kind": "bytes" }));
        }
        Ok(())
    }

    /// Send the Enter key to the session.
    pub fn send_enter(&self, session: SessionHandle) -> Result<()> {
        self.send_key(session, KeyEvent::Enter)
    }

    /// Read raw PTY output chunks since `seq` (exclusive).
    ///
    /// Returns `(chunks, next_seq)`. Pass `next_seq` as `seq` on the next call
    /// to get only new data.
    pub fn read_since(&self, session: SessionHandle, seq: u64) -> Result<(Vec<OutputChunk>, u64)> {
        let s = self.get_session(session)?;
        let sb = s.scrollback.lock().unwrap();
        Ok(sb.read_since(seq))
    }

    pub fn resize(&self, session: SessionHandle, size: TerminalSize) -> Result<()> {
        let s = self.get_session(session)?;
        let mut m = s.master.lock().unwrap();
        backend::resize(
            m.as_mut(),
            PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            },
        )?;
        if let Ok(mut sm) = s.screen_model.lock() {
            sm.resize(size.rows, size.cols);
        }
        if let Ok(mut info) = s.info.lock() {
            info.size = size;
        }
        if let Some(logger) = &s.logger {
            logger.log("Resized", &json!({ "rows": size.rows, "cols": size.cols }));
        }
        Ok(())
    }

    pub fn interrupt(&self, session: SessionHandle) -> Result<()> {
        let s = self.get_session(session)?;
        let mut w = s.writer.lock().unwrap();
        w.write_all(&[0x03])
            .map_err(|e| CoreError::Msg(e.to_string()))?;
        w.flush().ok();
        if let Some(logger) = &s.logger {
            logger.log("Interrupted", &json!({ "mode": "ctrl_c" }));
        }
        Ok(())
    }

    pub fn terminate(&self, session: SessionHandle) -> Result<()> {
        let s = self.get_session(session)?;
        if let Ok(mut killer) = s.killer.lock() {
            let _ = killer.kill();
        }
        if let Ok(mut info) = s.info.lock() {
            info.status = SessionStatus::Exited;
        }
        if let Some(logger) = &s.logger {
            logger.log("Terminated", &json!({}));
        }
        Ok(())
    }

    pub fn get_state(&self, session: SessionHandle) -> Result<SessionInfo> {
        let s = self.get_session(session)?;
        let info = s.info.lock().unwrap().clone();
        Ok(info)
    }

    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        let map = self.sessions.lock().unwrap();
        map.values()
            .map(|s| s.info.lock().unwrap().clone())
            .collect()
    }

    fn get_session(&self, session: SessionHandle) -> Result<Arc<Session>> {
        let map = self.sessions.lock().unwrap();
        map.get(&session.0)
            .cloned()
            .ok_or_else(|| CoreError::Msg("unknown session".into()))
    }

    pub fn send_key(&self, session: SessionHandle, key: KeyEvent) -> Result<()> {
        let s = self.get_session(session)?;
        let state = s.negotiator.lock().unwrap().state().clone();
        let bytes = encode_key(key, &state);
        let mut w = s.writer.lock().unwrap();
        w.write_all(&bytes)
            .map_err(|e| CoreError::Msg(e.to_string()))?;
        w.flush().ok();
        if let Some(logger) = &s.logger {
            logger.log(
                "InputSent",
                &json!({ "len": bytes.len(), "kind": "key", "key": format!("{key:?}") }),
            );
        }
        Ok(())
    }

    pub fn terminal_state(&self, session: SessionHandle) -> Result<TerminalState> {
        let s = self.get_session(session)?;
        let state = s.negotiator.lock().unwrap().state().clone();
        Ok(state)
    }

    pub fn input_profile_name(&self, session: SessionHandle) -> Result<Option<String>> {
        let s = self.get_session(session)?;
        Ok(s.input_profile_name.clone())
    }

    /// Subscribe to the broadcast channel of [`SemanticEvent`]s for this session.
    pub fn subscribe_events(
        &self,
        session: SessionHandle,
    ) -> Result<tokio::sync::broadcast::Receiver<SemanticEvent>> {
        let s = self.get_session(session)?;
        Ok(s.event_tx.subscribe())
    }

    pub fn agent_state(&self, session: SessionHandle) -> Result<AgentState> {
        let s = self.get_session(session)?;
        let cl = s.classifier.lock().unwrap();
        Ok(cl.state())
    }

    pub fn content_text(&self, session: SessionHandle) -> Result<String> {
        let s = self.get_session(session)?;
        let sm = s.screen_model.lock().unwrap();
        Ok(sm.content_text())
    }

    pub fn status_text(&self, session: SessionHandle) -> Result<String> {
        let s = self.get_session(session)?;
        let sm = s.screen_model.lock().unwrap();
        Ok(sm.status_text())
    }

    pub fn content_since_seq(&self, session: SessionHandle, seq: u64) -> Result<Vec<ContentEntry>> {
        let s = self.get_session(session)?;
        let cb = s.content_buffer.lock().unwrap();
        Ok(cb.since_seq(seq).into_iter().cloned().collect())
    }

    pub fn content_since_last_input(&self, session: SessionHandle) -> Result<Vec<ContentEntry>> {
        let s = self.get_session(session)?;
        let cb = s.content_buffer.lock().unwrap();
        Ok(cb.since_last_input().into_iter().cloned().collect())
    }

    pub fn content_text_since_last_input(&self, session: SessionHandle) -> Result<String> {
        let s = self.get_session(session)?;
        let cb = s.content_buffer.lock().unwrap();
        Ok(cb.text_since_last_input())
    }

    pub fn content_current_seq(&self, session: SessionHandle) -> Result<u64> {
        let s = self.get_session(session)?;
        let cb = s.content_buffer.lock().unwrap();
        Ok(cb.current_seq())
    }

    /// Subscribe to the broadcast channel of pilot-friendly [`WatchEvent`]s for this session.
    pub fn subscribe_watch_events(
        &self,
        session: SessionHandle,
    ) -> Result<tokio::sync::broadcast::Receiver<WatchEvent>> {
        let s = self.get_session(session)?;
        Ok(s.watch_event_tx.subscribe())
    }
}

// --- Content filter support ---
use crate::vte::ContentFilter;

impl SessionManager {
    pub fn filtered_content_since_last_input(&self, session: SessionHandle) -> Result<String> {
        let s = self.get_session(session)?;
        let filter: Box<dyn ContentFilterTrait> = s
            .adapter
            .as_ref()
            .map(|a| a.content_filter())
            .unwrap_or_else(|| Box::new(ContentFilter::new()));
        let cb = s.content_buffer.lock().unwrap();
        Ok(cb.filtered_text_since_last_input(filter.as_ref()))
    }

    pub fn filtered_content_since_seq(&self, session: SessionHandle, seq: u64) -> Result<String> {
        let s = self.get_session(session)?;
        let filter: Box<dyn ContentFilterTrait> = s
            .adapter
            .as_ref()
            .map(|a| a.content_filter())
            .unwrap_or_else(|| Box::new(ContentFilter::new()));
        let cb = s.content_buffer.lock().unwrap();
        Ok(cb.filtered_text_since_seq(seq, filter.as_ref()))
    }

    /// Snapshot the current visible content region from the VTE screen model
    /// and return it with TUI chrome stripped — the final-state source of
    /// truth for "what's on screen right now". Unlike the content-buffer
    /// methods, this cannot return duplicated progressive fragments because
    /// the screen model overwrites each row in place as it's rendered.
    ///
    /// Limitation: content that has scrolled off the visible screen is not
    /// included. For responses that fit in the content region, this returns
    /// the clean assistant text. For longer responses, the content buffer
    /// methods remain the right tool.
    /// Row-aware snapshot: walks the content buffer since the last input
    /// boundary and collapses same-row entries to their latest value. This
    /// is the correct primitive for ink/React TUIs (Claude Code, OpenCode)
    /// that re-render the entire content region on every token — it returns
    /// the final per-row state rather than the full write history.
    pub fn filtered_response_since_last_input(&self, session: SessionHandle) -> Result<String> {
        let s = self.get_session(session)?;
        let filter: Box<dyn ContentFilterTrait> = s
            .adapter
            .as_ref()
            .map(|a| a.content_filter())
            .unwrap_or_else(|| Box::new(ContentFilter::new()));
        let cb = s.content_buffer.lock().unwrap();
        Ok(cb.filtered_text_latest_per_row_since_last_input(filter.as_ref()))
    }

    pub fn filtered_screen_content(&self, session: SessionHandle) -> Result<String> {
        use crate::vte::content_filter::extract_response_text;
        let s = self.get_session(session)?;
        let filter: Box<dyn ContentFilterTrait> = s
            .adapter
            .as_ref()
            .map(|a| a.content_filter())
            .unwrap_or_else(|| Box::new(ContentFilter::new()));
        let sm = s.screen_model.lock().unwrap();
        let text = sm.content_text();
        drop(sm);
        let lines: Vec<String> = text.lines().filter_map(|l| filter.filter_line(l)).collect();
        Ok(extract_response_text(lines).join("\n"))
    }
}
