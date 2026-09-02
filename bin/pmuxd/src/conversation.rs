//! Sticky conversation leases for the Path B Messages facade.
//!
//! A Pi (or any Anthropic Messages client) sends the full `messages[]` every
//! turn. This module pins one pool instance to one conversation id and types
//! only the new suffix, so Claude Code's own Anthropic request keeps a stable
//! prefix and prompt-cache hits.
//!
//! The cell itself is a Path B pool instance. Between turns it sits `Leased`
//! (no `/clear`). Release or the pool's idle TTL runs `/clear` and returns
//! the instance to the idle set. The pool clock is the owner; a replay
//! refreshes `idle_since_ms`. Recycle is lease-end only.
//!
//! Harness contract (also implemented by `examples/pi/pmux.ts`):
//!
//! - Request header `x-pmux-conversation: <id>` (harness session id).
//!   `x-session-id` and `x-session-affinity` are accepted aliases.
//! - Response headers `x-pmux-conversation`, `x-pmux-cell`,
//!   `x-pmux-lease: primed|continued|reprimed|replayed`, `x-pmux-idle-ttl-ms`.
//! - `POST /v1/conversations/<id>/release` on session end. Idle TTL is the
//!   backstop. `/clear` happens here, not after every HTTP request.
//!
//! An implicit hash of the first user message is off unless the operator
//! starts the listener with `--messages-allow-implicit`. You did
//! not choose that id; release using the `x-pmux-conversation` the response
//! echoed. Two sessions that start the same way share a cell.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pseudomux_protocol::v1::{
    EffortLevel, ErrorBody, ErrorCode, RunStatelessRequest, StatelessResult,
};
use pseudomux_service::driver_io::validate_prompt;
use pseudomux_service::pool::{ModelEffortRefusal, Pool, resolve_pool_class};
use pseudomux_service::v1::DriverFailure;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::messages_http::{
    flatten_prompt, render_content, sanitize_prompt, split_model_and_effort, system_text,
};

const CONVERSATION_HEADER: &str = "x-pmux-conversation";

/// Operator knobs for the lease book. The pool owns the cells and the idle TTL.
#[derive(Clone, Debug)]
pub struct ConversationConfig {
    /// Advertised as `x-pmux-idle-ttl-ms`. Same duration as the pool clock;
    /// the book does not expire cells on its own `Instant`.
    pub idle_ttl: Duration,
    pub max_leases: u32,
    /// When false (the default), POST /v1/messages without an explicit pin
    /// is refused. When true, the first-turn hash is used instead.
    pub allow_implicit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseKind {
    Primed,
    Continued,
    Reprimed,
    Replayed,
}

impl LeaseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primed => "primed",
            Self::Continued => "continued",
            Self::Reprimed => "reprimed",
            Self::Replayed => "replayed",
        }
    }
}

pub struct LeaseTurn {
    pub conversation_id: String,
    pub cell: String,
    pub kind: LeaseKind,
    pub idle_ttl_ms: u64,
    /// The `model` string the request carried, byte for byte
    /// (`claude-opus-5-xhigh`, or an alias). This is what the Messages
    /// response echoes: a harness that verifies "the child answered with the
    /// model I launched" compares against the id IT sent, and pi-subagents
    /// 0.63 refused every pmux child with `model_verification_failed`
    /// (expected `pmux/claude-opus-5-xhigh`, observed `claude-opus-5`) when
    /// the response named the canonical stem instead. Measured on macos
    /// 2026-09-01 at Claude Code 2.1.258. The canonical stem is
    /// `result.model`, so nothing is lost by echoing the caller's spelling.
    pub requested_model: String,
    pub result: StatelessResult,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationFingerprint {
    pub model: String,
    pub effort: Option<EffortLevel>,
    pub system_tools: String,
    pub messages: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefixDecision {
    Continue { from: usize },
    Replay,
    Reprime,
}

#[must_use]
pub fn classify_prefix(
    previous: &ConversationFingerprint,
    next: &ConversationFingerprint,
) -> PrefixDecision {
    if pool_class_changed(previous, next) {
        return PrefixDecision::Reprime;
    }
    if previous.system_tools != next.system_tools {
        return PrefixDecision::Reprime;
    }
    if next.messages == previous.messages {
        return PrefixDecision::Replay;
    }
    if next.messages.starts_with(&previous.messages)
        && next.messages.len() > previous.messages.len()
    {
        return PrefixDecision::Continue {
            from: previous.messages.len(),
        };
    }
    PrefixDecision::Reprime
}

fn pool_class_changed(previous: &ConversationFingerprint, next: &ConversationFingerprint) -> bool {
    match (
        resolve_pool_class(&previous.model, previous.effort),
        resolve_pool_class(&next.model, next.effort),
    ) {
        (Ok((left, _)), Ok((right, _))) => left != right,
        _ => previous.model != next.model || previous.effort != next.effort,
    }
}

pub fn fingerprint_body(
    body: &Value,
    model: &str,
    effort: Option<EffortLevel>,
) -> ConversationFingerprint {
    let system = system_text(body.get("system"));
    let tools = body
        .get("tools")
        .map(std::string::ToString::to_string)
        .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(system.as_bytes());
    hasher.update([0]);
    hasher.update(tools.as_bytes());
    let system_tools = hex(hasher.finalize());
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|message| {
                    let role = message
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("user");
                    let rendered = render_content(message.get("content"));
                    sha_hex(&format!("{role}\n{rendered}"))
                })
                .collect()
        })
        .unwrap_or_default();
    ConversationFingerprint {
        model: model.to_owned(),
        effort,
        system_tools,
        messages,
    }
}

/// Conversation id from an explicit pin header, if one was sent.
pub fn explicit_conversation_id(headers: &[(String, String)]) -> Option<String> {
    for name in [CONVERSATION_HEADER, "x-session-id", "x-session-affinity"] {
        if let Some(explicit) = header(headers, name) {
            let trimmed = explicit.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

/// Pins are path segments on `POST /v1/conversations/{id}/release`.
/// `/`, whitespace, `?`, and `#` cannot be named there; the daemon does
/// not percent-decode, so those characters are refused at the pin too.
pub fn require_path_safe_conversation_id(id: &str) -> Result<String, ErrorBody> {
    let id = id.trim();
    if id.is_empty() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "conversation id must not be empty",
        ));
    }
    if id.contains(['/', '?', '#']) || id.chars().any(char::is_whitespace) {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "conversation id is not path-safe",
        ));
    }
    Ok(id.to_owned())
}

/// Conversation id from the header, or an implicit hash of the first turn.
///
/// The implicit arm is the operator opt-in. Production harnesses send a pin.
pub fn conversation_id_from(
    headers: &[(String, String)],
    body: &Value,
    model: &str,
    effort: Option<EffortLevel>,
    allow_implicit: bool,
) -> Result<String, ErrorBody> {
    if let Some(explicit) = explicit_conversation_id(headers) {
        return require_path_safe_conversation_id(&explicit);
    }
    if allow_implicit {
        return Ok(implicit_conversation_id(body, model, effort));
    }
    Err(missing_conversation_header())
}

pub fn missing_conversation_header() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidConfig,
        "a conversation pin is required: send x-pmux-conversation (or x-session-id / x-session-affinity) on every POST /v1/messages",
    )
    .advising(
        "set x-pmux-conversation to the harness session id, or start pmuxd with --messages-allow-implicit for a single-session implicit hash",
    )
}

pub fn implicit_conversation_id(body: &Value, model: &str, effort: Option<EffortLevel>) -> String {
    let first = body
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .map(|message| {
            format!(
                "{}\n{}",
                message
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("user"),
                render_content(message.get("content"))
            )
        })
        .unwrap_or_default();
    let system = system_text(body.get("system"));
    let tools = body
        .get("tools")
        .map(std::string::ToString::to_string)
        .unwrap_or_default();
    let effort_token = effort.map(EffortLevel::as_str).unwrap_or("-");
    sha_hex(&format!(
        "{model}\n{effort_token}\n{system}\n{tools}\n{first}"
    ))
}

pub fn continuation_prompt(body: &Value, from: usize) -> Result<String, String> {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "messages is required".to_owned())?;
    if from >= messages.len() {
        return Err("continuation produced an empty prompt".to_owned());
    }
    let mut out = String::from("HISTORY:\n");
    for message in &messages[from..] {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        out.push_str(match role {
            "assistant" => "ASSISTANT:\n",
            "system" => "SYSTEM:\n",
            _ => "USER:\n",
        });
        out.push_str(&render_content(message.get("content")));
        out.push_str("\n\n");
    }
    out.push_str(
        "Continue as the assistant. Either answer in plain text or emit tool_call blocks.\n",
    );
    let prompt = sanitize_prompt(&out);
    if prompt.trim().is_empty() {
        return Err("continuation produced an empty prompt".to_owned());
    }
    Ok(prompt)
}

struct Lease {
    fingerprint: ConversationFingerprint,
    last_result: StatelessResult,
    last_used: Instant,
    in_flight: bool,
    cell: String,
}

struct BookState {
    live: HashMap<String, Lease>,
    reserved: HashSet<String>,
}

impl BookState {
    fn occupied(&self) -> usize {
        self.live.len().saturating_add(self.reserved.len())
    }
}

pub struct ConversationBook {
    config: ConversationConfig,
    pool: Arc<Pool>,
    leases: Mutex<BookState>,
}

impl ConversationBook {
    #[must_use]
    pub fn new(config: ConversationConfig, pool: Arc<Pool>) -> Self {
        Self {
            config,
            pool,
            leases: Mutex::new(BookState {
                live: HashMap::new(),
                reserved: HashSet::new(),
            }),
        }
    }

    pub fn idle_ttl_ms(&self) -> u64 {
        u64::try_from(self.config.idle_ttl.as_millis()).unwrap_or(u64::MAX)
    }

    #[must_use]
    pub fn allows_implicit(&self) -> bool {
        self.config.allow_implicit
    }

    #[cfg(test)]
    async fn last_used_for_test(&self, conversation_id: &str) -> Option<Instant> {
        self.leases
            .lock()
            .await
            .live
            .get(conversation_id)
            .map(|lease| lease.last_used)
    }

    pub async fn complete(
        &self,
        headers: &[(String, String)],
        body: &Value,
    ) -> Result<LeaseTurn, ErrorBody> {
        let raw_model = body
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| ErrorBody::new(ErrorCode::InvalidConfig, "model is required"))?;
        let (model, effort_from_id) = split_model_and_effort(raw_model);
        let effort = effort_from_id.or(effort_from_body(body)?);
        let (class, resolved) =
            resolve_pool_class(&model, effort).map_err(ModelEffortRefusal::into_error_body)?;
        let requested_model = raw_model.to_owned();
        let model = class.canonical_model.to_owned();
        let effort = resolved.effort_level;
        let conversation_id =
            conversation_id_from(headers, body, &model, effort, self.config.allow_implicit)?;
        let next = fingerprint_body(body, &model, effort);
        // Flatten for Prime / Reprime / SessionNotFound fallback. Continue
        // validates only the suffix; Replay types nothing.
        let primer = sanitize_prompt(
            &flatten_prompt(body)
                .map_err(|message| ErrorBody::new(ErrorCode::InvalidConfig, message))?,
        );

        let pool_ids = self.pool_conversation_ids().await;
        let planned = {
            let mut guard = self.leases.lock().await;
            self.drop_orphans(&mut guard, &pool_ids);

            let existing = guard.live.get(&conversation_id).map(|lease| {
                (
                    lease.in_flight,
                    classify_prefix(&lease.fingerprint, &next),
                    lease.cell.clone(),
                    lease.last_result.clone(),
                )
            });
            let mut planned = None;
            if let Some((in_flight, decision, cell, last_result)) = existing {
                if in_flight {
                    return Err(session_busy_in_flight());
                }
                match decision {
                    PrefixDecision::Replay => {
                        if self.pool.touch_conversation(&conversation_id).await {
                            if let Some(lease) = guard.live.get_mut(&conversation_id) {
                                lease.last_used = Instant::now();
                            }
                            return Ok(LeaseTurn {
                                conversation_id,
                                cell,
                                kind: LeaseKind::Replayed,
                                idle_ttl_ms: self.idle_ttl_ms(),
                                requested_model,
                                result: last_result,
                            });
                        }
                        guard.live.remove(&conversation_id);
                    }
                    PrefixDecision::Continue { from } => {
                        if let Some(lease) = guard.live.get_mut(&conversation_id) {
                            lease.in_flight = true;
                        }
                        self.pool.protect_conversation(&conversation_id).await;
                        planned = Some(Planned {
                            conversation_id: conversation_id.clone(),
                            model: model.clone(),
                            requested_model: requested_model.clone(),
                            effort,
                            next: next.clone(),
                            kind: PlannedKind::Continue { from },
                        });
                    }
                    PrefixDecision::Reprime => {
                        if let Some(lease) = guard.live.get_mut(&conversation_id) {
                            lease.in_flight = true;
                        }
                        planned = Some(Planned {
                            conversation_id: conversation_id.clone(),
                            model: model.clone(),
                            requested_model: requested_model.clone(),
                            effort,
                            next: next.clone(),
                            kind: PlannedKind::Reprime,
                        });
                    }
                }
            }
            if let Some(planned) = planned {
                planned
            } else if guard.reserved.contains(&conversation_id) {
                return Err(session_busy_in_flight());
            } else {
                if u32::try_from(guard.occupied()).unwrap_or(u32::MAX) >= self.config.max_leases {
                    return Err(session_busy_cap(self.config.max_leases));
                }
                guard.reserved.insert(conversation_id.clone());
                Planned {
                    conversation_id: conversation_id.clone(),
                    model: model.clone(),
                    requested_model: requested_model.clone(),
                    effort,
                    next,
                    kind: PlannedKind::Prime,
                }
            }
        };

        let kind = planned.kind;
        let outcome = self.execute(planned, body, &primer).await;
        self.pool.unprotect_conversation(&conversation_id).await;
        if let Err(error) = &outcome {
            self.recover_failed_turn(&conversation_id, kind, error.code)
                .await;
        }
        outcome
    }

    pub async fn release(&self, conversation_id: &str) -> Result<(), ErrorBody> {
        {
            let mut guard = self.leases.lock().await;
            if guard.reserved.contains(conversation_id)
                || guard
                    .live
                    .get(conversation_id)
                    .is_some_and(|lease| lease.in_flight)
            {
                return Err(session_busy_in_flight());
            }
            guard.live.remove(conversation_id);
        }
        self.pool.release_conversation(conversation_id).await
    }

    pub async fn sweep_expired(&self) {
        let pool_ids = self.pool_conversation_ids().await;
        let mut guard = self.leases.lock().await;
        self.drop_orphans(&mut guard, &pool_ids);
    }

    async fn pool_conversation_ids(&self) -> HashSet<String> {
        self.pool
            .conversation_leases()
            .await
            .into_iter()
            .map(|lease| lease.conversation_id)
            .collect()
    }

    /// Drop book rows whose pool cell is gone. Orphans do not occupy `max_leases`.
    fn drop_orphans(&self, guard: &mut BookState, pool_ids: &HashSet<String>) {
        let orphans: Vec<String> = guard
            .live
            .iter()
            .filter(|(id, lease)| !lease.in_flight && !pool_ids.contains(*id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in orphans {
            guard.live.remove(&id);
        }
    }

    /// A refused suffix must not `/clear` a still-leased cell. A failed prime
    /// or a pool teardown drops the book row; `release_conversation` is
    /// success if the cell is already gone.
    async fn recover_failed_turn(&self, conversation_id: &str, kind: PlannedKind, code: ErrorCode) {
        let keep_leased_continue = matches!(kind, PlannedKind::Continue { .. })
            && code == ErrorCode::InvalidConfig
            && self.pool_conversation_ids().await.contains(conversation_id);
        let mut guard = self.leases.lock().await;
        if keep_leased_continue {
            if let Some(lease) = guard.live.get_mut(conversation_id) {
                lease.in_flight = false;
            }
            return;
        }
        guard.reserved.remove(conversation_id);
        guard.live.remove(conversation_id);
        drop(guard);
        let _ = self.pool.release_conversation(conversation_id).await;
    }

    async fn execute(
        &self,
        planned: Planned,
        body: &Value,
        primer: &str,
    ) -> Result<LeaseTurn, ErrorBody> {
        let conversation_id = planned.conversation_id.clone();
        let model = planned.model.clone();
        let request_for = |prompt: String| RunStatelessRequest {
            model: model.clone(),
            effort: planned.effort,
            prompt,
            deadline_unix_ms: None,
        };

        let (kind, turn) = match planned.kind {
            PlannedKind::Prime => {
                validate_typed_prompt(primer)?;
                (
                    LeaseKind::Primed,
                    self.pool
                        .run_sticky(&conversation_id, request_for(primer.to_owned()), false)
                        .await?,
                )
            }
            PlannedKind::Continue { from } => {
                let prompt = continuation_prompt(body, from)
                    .map_err(|message| ErrorBody::new(ErrorCode::InvalidConfig, message))?;
                validate_typed_prompt(&prompt)?;
                match self
                    .pool
                    .run_sticky(&conversation_id, request_for(prompt), true)
                    .await
                {
                    Ok(turn) => (LeaseKind::Continued, turn),
                    Err(error) if error.code == ErrorCode::SessionNotFound => {
                        validate_typed_prompt(primer)?;
                        let turn = self
                            .pool
                            .run_sticky(&conversation_id, request_for(primer.to_owned()), false)
                            .await?;
                        (LeaseKind::Reprimed, turn)
                    }
                    Err(error) => return Err(error),
                }
            }
            PlannedKind::Reprime => {
                validate_typed_prompt(primer)?;
                let _ = self.pool.release_conversation(&conversation_id).await;
                (
                    LeaseKind::Reprimed,
                    self.pool
                        .run_sticky(&conversation_id, request_for(primer.to_owned()), false)
                        .await?,
                )
            }
        };

        {
            let mut guard = self.leases.lock().await;
            guard.reserved.remove(&conversation_id);
            guard.live.insert(
                conversation_id.clone(),
                Lease {
                    fingerprint: planned.next,
                    last_result: turn.result.clone(),
                    last_used: Instant::now(),
                    in_flight: false,
                    cell: turn.cell.clone(),
                },
            );
        }

        Ok(LeaseTurn {
            conversation_id,
            cell: turn.cell,
            kind,
            idle_ttl_ms: self.idle_ttl_ms(),
            requested_model: planned.requested_model,
            result: turn.result,
        })
    }
}

struct Planned {
    conversation_id: String,
    model: String,
    requested_model: String,
    effort: Option<EffortLevel>,
    next: ConversationFingerprint,
    kind: PlannedKind,
}

#[derive(Clone, Copy)]
enum PlannedKind {
    Prime,
    Continue { from: usize },
    Reprime,
}

fn validate_typed_prompt(prompt: &str) -> Result<(), ErrorBody> {
    validate_prompt(prompt).map_err(DriverFailure::into_protocol)?;
    Ok(())
}

fn session_busy_in_flight() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::SessionBusy,
        "this conversation already has a turn in flight; nothing is queued",
    )
    .retryable(true)
}

fn session_busy_cap(max_leases: u32) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::SessionBusy,
        format!(
            "conversation lease cap is {max_leases}; release an idle conversation or raise --pool-size"
        ),
    )
    .retryable(true)
}

fn effort_from_body(body: &Value) -> Result<Option<EffortLevel>, ErrorBody> {
    let raw = body
        .pointer("/output_config/effort")
        .and_then(Value::as_str)
        .or_else(|| {
            body.get("thinking")
                .and_then(Value::as_object)
                .and_then(|thinking| thinking.get("type"))
                .and_then(Value::as_str)
                .and_then(|kind| match kind {
                    "disabled" => None,
                    _ => Some("high"),
                })
        });
    match raw {
        None => Ok(None),
        Some(value) => serde_json::from_value::<EffortLevel>(Value::String(value.to_owned()))
            .map(Some)
            .map_err(|_| {
                ErrorBody::new(
                    ErrorCode::InvalidConfig,
                    format!("unsupported effort {value:?}"),
                )
            }),
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn sha_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex(hasher.finalize())
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(messages: Vec<Value>) -> Value {
        json!({
            "model": "claude-sonnet-5-low",
            "system": "Be terse.",
            "tools": [{"name":"read","input_schema":{"type":"object"}}],
            "messages": messages
        })
    }

    #[test]
    fn implicit_id_is_stable_as_history_grows() {
        let first = body(vec![json!({"role":"user","content":"hello"})]);
        let second = body(vec![
            json!({"role":"user","content":"hello"}),
            json!({"role":"assistant","content":"hi"}),
            json!({"role":"user","content":"again"}),
        ]);
        let (model, effort) = split_model_and_effort("claude-sonnet-5-low");
        assert_eq!(
            implicit_conversation_id(&first, &model, effort),
            implicit_conversation_id(&second, &model, effort)
        );
    }

    #[test]
    fn strict_extension_is_continue() {
        let (model, effort) = split_model_and_effort("claude-sonnet-5-low");
        let first = fingerprint_body(
            &body(vec![json!({"role":"user","content":"hello"})]),
            &model,
            effort,
        );
        let second = fingerprint_body(
            &body(vec![
                json!({"role":"user","content":"hello"}),
                json!({"role":"assistant","content":"hi"}),
                json!({"role":"user","content":"again"}),
            ]),
            &model,
            effort,
        );
        assert_eq!(
            classify_prefix(&first, &second),
            PrefixDecision::Continue { from: 1 }
        );
    }

    #[test]
    fn same_messages_replay() {
        let (model, effort) = split_model_and_effort("claude-sonnet-5-low");
        let fp = fingerprint_body(
            &body(vec![json!({"role":"user","content":"hello"})]),
            &model,
            effort,
        );
        assert_eq!(classify_prefix(&fp, &fp), PrefixDecision::Replay);
    }

    #[test]
    fn rewind_or_compact_reprimes() {
        let (model, effort) = split_model_and_effort("claude-sonnet-5-low");
        let long = fingerprint_body(
            &body(vec![
                json!({"role":"user","content":"one"}),
                json!({"role":"assistant","content":"a"}),
                json!({"role":"user","content":"two"}),
            ]),
            &model,
            effort,
        );
        let compacted = fingerprint_body(
            &body(vec![json!({"role":"user","content":"summary then two"})]),
            &model,
            effort,
        );
        assert_eq!(classify_prefix(&long, &compacted), PrefixDecision::Reprime);
    }

    #[test]
    fn class_change_reprimes() {
        let low = fingerprint_body(
            &body(vec![json!({"role":"user","content":"hello"})]),
            "claude-sonnet-5",
            Some(EffortLevel::Low),
        );
        let high = fingerprint_body(
            &body(vec![
                json!({"role":"user","content":"hello"}),
                json!({"role":"assistant","content":"hi"}),
                json!({"role":"user","content":"more"}),
            ]),
            "claude-sonnet-5",
            Some(EffortLevel::High),
        );
        assert_eq!(classify_prefix(&low, &high), PrefixDecision::Reprime);
    }

    #[test]
    fn continuation_prompt_is_only_the_suffix() {
        let body = body(vec![
            json!({"role":"user","content":"hello"}),
            json!({"role":"assistant","content":"hi"}),
            json!({"role":"user","content":"what is 2+2"}),
        ]);
        let prompt = continuation_prompt(&body, 2).unwrap();
        assert!(prompt.contains("what is 2+2"));
        assert!(!prompt.contains("hello"));
        assert!(prompt.contains("USER:"));
    }

    #[test]
    fn header_wins_over_implicit_id() {
        let body = body(vec![json!({"role":"user","content":"hello"})]);
        let (model, effort) = split_model_and_effort("claude-sonnet-5-low");
        let id = conversation_id_from(
            &[("X-Pmux-Conversation".to_owned(), "sess-1".to_owned())],
            &body,
            &model,
            effort,
            false,
        )
        .unwrap();
        assert_eq!(id, "sess-1");
    }

    #[test]
    fn an_explicit_pin_must_be_path_safe() {
        let body = body(vec![json!({"role":"user","content":"hello"})]);
        let (model, effort) = split_model_and_effort("claude-sonnet-5-low");
        for bad in ["a/b", "a b", "a?b", "a#b", "a\tb"] {
            let err = conversation_id_from(
                &[("x-pmux-conversation".to_owned(), bad.to_owned())],
                &body,
                &model,
                effort,
                false,
            )
            .unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidConfig, "{bad}");
            assert!(err.message.contains("path-safe"), "{bad}");
        }
    }

    #[test]
    fn a_missing_pin_is_refused_unless_implicit_is_allowed() {
        let body = body(vec![json!({"role":"user","content":"hello"})]);
        let (model, effort) = split_model_and_effort("claude-sonnet-5-low");
        let refused = conversation_id_from(&[], &body, &model, effort, false).unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidConfig);
        assert!(refused.message.contains("x-pmux-conversation"));
        let implicit = conversation_id_from(&[], &body, &model, effort, true).unwrap();
        assert_eq!(implicit, implicit_conversation_id(&body, &model, effort));
    }

    #[test]
    fn sonnet_then_claude_sonnet_5_continues_at_the_same_effort() {
        let first = fingerprint_body(
            &body(vec![json!({"role":"user","content":"hello"})]),
            "sonnet",
            Some(EffortLevel::Low),
        );
        let second = fingerprint_body(
            &json!({
                "model": "claude-sonnet-5-low",
                "system": "Be terse.",
                "tools": [{"name":"read","input_schema":{"type":"object"}}],
                "messages": [
                    {"role":"user","content":"hello"},
                    {"role":"assistant","content":"hi"},
                    {"role":"user","content":"again"}
                ]
            }),
            "claude-sonnet-5",
            Some(EffortLevel::Low),
        );
        assert_eq!(
            classify_prefix(&first, &second),
            PrefixDecision::Continue { from: 1 }
        );
    }

    #[test]
    fn opus_aliases_continue_at_the_same_effort() {
        let first = fingerprint_body(
            &json!({
                "model": "opus",
                "messages": [{"role":"user","content":"hello"}]
            }),
            "opus",
            Some(EffortLevel::High),
        );
        let second = fingerprint_body(
            &json!({
                "model": "claude-opus-5",
                "messages": [
                    {"role":"user","content":"hello"},
                    {"role":"assistant","content":"hi"},
                    {"role":"user","content":"again"}
                ]
            }),
            "claude-opus-5",
            Some(EffortLevel::High),
        );
        assert_eq!(
            classify_prefix(&first, &second),
            PrefixDecision::Continue { from: 1 }
        );
    }

    #[test]
    fn a_large_history_continue_validates_only_the_suffix() {
        use pseudomux_service::driver_io::MAX_PROMPT_BYTES;
        let huge = "a".repeat(MAX_PROMPT_BYTES);
        let history = vec![
            json!({"role":"user","content": huge}),
            json!({"role":"assistant","content":"ok"}),
        ];
        let mut continued = history.clone();
        continued.push(json!({"role":"user","content":"next"}));
        let next = json!({
            "model": "claude-sonnet-5-low",
            "messages": continued
        });
        let primer = sanitize_prompt(&flatten_prompt(&next).unwrap());
        assert!(primer.len() > MAX_PROMPT_BYTES);
        assert!(
            validate_prompt(&primer).is_err(),
            "the flattened primer must exceed the 1 MiB service limit"
        );
        let (model, effort) = split_model_and_effort("claude-sonnet-5-low");
        let previous = fingerprint_body(
            &json!({"model":"claude-sonnet-5-low","messages": history}),
            &model,
            effort,
        );
        let next_fp = fingerprint_body(&next, &model, effort);
        assert_eq!(
            classify_prefix(&previous, &next_fp),
            PrefixDecision::Continue { from: 2 }
        );
        let suffix = continuation_prompt(&next, 2).unwrap();
        assert!(validate_prompt(&suffix).is_ok());
        assert_eq!(classify_prefix(&next_fp, &next_fp), PrefixDecision::Replay);
        assert!(
            validate_prompt(&primer).is_err(),
            "Replay types nothing, so a 1 MiB primer must not be the check"
        );
    }

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use pseudomux_protocol::v1::{SessionGenerationId, SessionId, UsageBreakdown};
    use pseudomux_service::pool::{
        Destroyed, HostFailure, HostTurn, InstanceHandle, InstanceHost, MintSpec, PoolSettings,
        Spawner,
    };
    use pseudomux_service::v1::Clock;

    struct TestClock {
        now_ms: AtomicU64,
    }

    impl TestClock {
        fn advance(&self, delta: u64) {
            self.now_ms.fetch_add(delta, Ordering::Relaxed);
        }
    }

    impl Clock for TestClock {
        fn now_ms(&self) -> u64 {
            self.now_ms.load(Ordering::Relaxed)
        }
    }

    #[derive(Default)]
    struct QueueSpawner {
        queued:
            std::sync::Mutex<Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>>>,
    }

    impl QueueSpawner {
        async fn drain(&self) {
            loop {
                let batch: Vec<_> = std::mem::take(&mut *self.queued.lock().expect("spawner lock"));
                if batch.is_empty() {
                    return;
                }
                for work in batch {
                    work.await;
                }
            }
        }
    }

    impl Spawner for QueueSpawner {
        fn spawn(&self, work: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>) {
            self.queued.lock().expect("spawner lock").push(work);
        }
    }

    struct BookHost;

    #[async_trait]
    impl InstanceHost for BookHost {
        async fn mint(&self, spec: MintSpec) -> Result<InstanceHandle, HostFailure> {
            let mut bytes = [0_u8; 16];
            bytes[0..4].copy_from_slice(&spec.slot.to_be_bytes());
            bytes[4..12].copy_from_slice(&spec.epoch.to_be_bytes());
            Ok(InstanceHandle {
                session_id: SessionId::from_bytes(bytes),
                generation_id: SessionGenerationId::default(),
                pid: Some(1_000),
                claude_version: "2.1.220".to_owned(),
            })
        }

        async fn run_turn(
            &self,
            _handle: &InstanceHandle,
            prompt: String,
            _deadline_unix_ms: u64,
        ) -> Result<HostTurn, HostFailure> {
            Ok(HostTurn {
                text: format!("answered: {prompt}"),
                reported_model: None,
                stop_reason: None,
                usage: UsageBreakdown::default(),
                sidechain_rows: Some(0),
            })
        }

        async fn clear(
            &self,
            _handle: &InstanceHandle,
        ) -> Result<(), pseudomux_service::pool::ClearFailure> {
            Ok(())
        }

        async fn destroy(&self, _handle: &InstanceHandle) -> Result<Destroyed, HostFailure> {
            Ok(Destroyed {
                process_reaped: true,
            })
        }
    }

    struct BookHarness {
        book: ConversationBook,
        pool: std::sync::Arc<Pool>,
        clock: std::sync::Arc<TestClock>,
        spawner: std::sync::Arc<QueueSpawner>,
        _temp: tempfile::TempDir,
    }

    fn book_harness(pool_size: u32, ttl_ms: u64) -> BookHarness {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("pool");
        let mut settings = PoolSettings::defaults(parent, PathBuf::from("/usr/bin/claude"));
        settings.pool_size = pool_size;
        settings.rss_budget_mb = u64::from(pool_size) * 1024;
        settings.instance_idle_ttl_ms = ttl_ms;
        let config = settings.validate().expect("test settings must validate");
        let clock = std::sync::Arc::new(TestClock {
            now_ms: AtomicU64::new(1_000),
        });
        let spawner = std::sync::Arc::new(QueueSpawner::default());
        let pool = Pool::new(
            config,
            std::sync::Arc::new(BookHost) as std::sync::Arc<dyn InstanceHost>,
            std::sync::Arc::clone(&clock) as std::sync::Arc<dyn Clock>,
            std::sync::Arc::clone(&spawner) as std::sync::Arc<dyn Spawner>,
        );
        let book = ConversationBook::new(
            ConversationConfig {
                idle_ttl: Duration::from_millis(ttl_ms),
                max_leases: pool_size,
                allow_implicit: false,
            },
            std::sync::Arc::clone(&pool),
        );
        BookHarness {
            book,
            pool,
            clock,
            spawner,
            _temp: temp,
        }
    }

    fn pin(id: &str) -> Vec<(String, String)> {
        vec![("x-pmux-conversation".to_owned(), id.to_owned())]
    }

    fn turn(model: &str, messages: Vec<Value>) -> Value {
        json!({ "model": model, "messages": messages })
    }

    #[tokio::test]
    async fn replay_after_the_pool_ttl_keeps_the_cell_because_replay_touches() {
        let harness = book_harness(2, 1_000);
        let first = turn(
            "claude-sonnet-5-low",
            vec![json!({"role":"user","content":"hello"})],
        );
        let primed = harness
            .book
            .complete(&pin("sess-a"), &first)
            .await
            .expect("prime");
        assert_eq!(primed.kind, LeaseKind::Primed);
        let used_before = harness
            .book
            .last_used_for_test("sess-a")
            .await
            .expect("primed row");
        harness.clock.advance(10_000);
        let replayed = harness
            .book
            .complete(&pin("sess-a"), &first)
            .await
            .expect("replay is activity");
        assert_eq!(replayed.kind, LeaseKind::Replayed);
        let used_after = harness
            .book
            .last_used_for_test("sess-a")
            .await
            .expect("replayed row");
        assert!(used_after >= used_before);
        harness.pool.sweep_idle().await;
        harness.spawner.drain().await;
        assert_eq!(
            harness.pool.census().await.leased,
            1,
            "touch must refresh idle_since_ms so the pool TTL does not /clear"
        );
        let again = harness
            .book
            .complete(&pin("sess-a"), &first)
            .await
            .expect("book row still valid");
        assert_eq!(again.kind, LeaseKind::Replayed);
    }

    #[tokio::test]
    async fn orphan_book_rows_do_not_occupy_the_lease_cap() {
        let harness = book_harness(15, 60_000);
        for index in 0..15 {
            let id = format!("sess-{index}");
            harness
                .book
                .complete(
                    &pin(&id),
                    &turn(
                        "claude-sonnet-5-low",
                        vec![json!({"role":"user","content":"hello"})],
                    ),
                )
                .await
                .expect("prime");
        }
        for index in 0..15 {
            harness
                .pool
                .release_conversation(&format!("sess-{index}"))
                .await
                .expect("drop the pool cell, leave the book row");
        }
        harness.spawner.drain().await;
        let next = harness
            .book
            .complete(
                &pin("sess-new"),
                &turn(
                    "claude-sonnet-5-low",
                    vec![json!({"role":"user","content":"fresh"})],
                ),
            )
            .await
            .expect("orphans must not occupy max_leases");
        assert_eq!(next.kind, LeaseKind::Primed);
    }

    #[tokio::test]
    async fn a_large_history_continue_does_not_fail_the_primer_limit() {
        use pseudomux_service::driver_io::MAX_PROMPT_BYTES;
        let harness = book_harness(2, 60_000);
        let block = "a".repeat(600_000);
        let first = turn(
            "claude-sonnet-5-low",
            vec![json!({"role":"user","content": block})],
        );
        harness
            .book
            .complete(&pin("sess-big"), &first)
            .await
            .expect("prime stays under 1 MiB");
        let next = turn(
            "claude-sonnet-5-low",
            vec![
                json!({"role":"user","content": block}),
                json!({"role":"assistant","content": block}),
                json!({"role":"user","content":"next"}),
            ],
        );
        let primer = sanitize_prompt(&flatten_prompt(&next).unwrap());
        assert!(primer.len() > MAX_PROMPT_BYTES);
        assert!(validate_prompt(&primer).is_err());
        let continued = harness
            .book
            .complete(&pin("sess-big"), &next)
            .await
            .expect("Continue must not validate the flattened primer");
        assert_eq!(continued.kind, LeaseKind::Continued);
        let replayed = harness
            .book
            .complete(&pin("sess-big"), &next)
            .await
            .expect("Replay of a large history must skip primer validation");
        assert_eq!(replayed.kind, LeaseKind::Replayed);
    }

    #[tokio::test]
    async fn a_refused_suffix_does_not_release_the_leased_cell() {
        let harness = book_harness(2, 60_000);
        let first = turn(
            "claude-sonnet-5-low",
            vec![json!({"role":"user","content":"hello"})],
        );
        harness
            .book
            .complete(&pin("sess-keep"), &first)
            .await
            .expect("prime");
        let next = turn(
            "claude-sonnet-5-low",
            vec![
                json!({"role":"user","content":"hello"}),
                json!({"role":"assistant","content":"hi"}),
                json!({
                    "role":"user",
                    "content": "x".repeat(pseudomux_service::driver_io::MAX_PROMPT_BYTES + 1)
                }),
            ],
        );
        let error = match harness.book.complete(&pin("sess-keep"), &next).await {
            Ok(_) => panic!("an oversized suffix must be InvalidConfig"),
            Err(error) => error,
        };
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert_eq!(
            harness.pool.census().await.leased,
            1,
            "a refused suffix must not /clear the still-leased cell"
        );
        let replayed = harness
            .book
            .complete(&pin("sess-keep"), &first)
            .await
            .expect("the book row and cell must still be there");
        assert_eq!(replayed.kind, LeaseKind::Replayed);
    }
}
