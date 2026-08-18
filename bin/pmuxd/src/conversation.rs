//! Sticky conversation leases for the Path B Messages facade.
//!
//! A Pi (or any Anthropic Messages client) sends the full `messages[]` every
//! turn. This module pins one pool instance to one conversation id and types
//! only the new suffix, so Claude Code's own Anthropic request keeps a stable
//! prefix and prompt-cache hits.
//!
//! The cell itself is a Path B pool instance. Between turns it sits `Leased`
//! (no `/clear`). Release or idle TTL runs `/clear` and returns the instance
//! to the idle set.
//!
//! Harness contract (also implemented by `~/.pi/agent/extensions/pmux.ts`):
//!
//! - Request header `x-pmux-conversation: <id>` (Pi session id, or a UUID
//!   for `pi -p --no-session`).
//! - Response headers `x-pmux-conversation`, `x-pmux-cell`,
//!   `x-pmux-lease: primed|continued|reprimed|replayed`, `x-pmux-idle-ttl-ms`.
//! - `POST /v1/conversations/<id>/release` on session end. Idle TTL is the
//!   backstop. `/clear` happens here, not after every HTTP request.
//!
//! Unaware clients still work: the first user message (plus model/effort/
//! system/tools) is hashed into an implicit id. They cannot eagerly release.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pseudomux_protocol::v1::{
    EffortLevel, ErrorBody, ErrorCode, RunStatelessRequest, StatelessResult,
};
use pseudomux_service::driver_io::validate_prompt;
use pseudomux_service::pool::Pool;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::messages_http::{
    flatten_prompt, render_content, sanitize_prompt, split_model_and_effort, system_text,
};

const CONVERSATION_HEADER: &str = "x-pmux-conversation";

/// Operator knobs for the lease book. The pool owns the cells.
#[derive(Clone, Debug)]
pub struct ConversationConfig {
    pub idle_ttl: Duration,
    pub max_leases: u32,
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
    pub model: String,
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
    if previous.model != next.model || previous.effort != next.effort {
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
    let system_tools = hex(&hasher.finalize());
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

/// Conversation id from the header, or an implicit hash of the first turn.
pub fn conversation_id_from(
    headers: &[(String, String)],
    body: &Value,
    model: &str,
    effort: Option<EffortLevel>,
) -> String {
    if let Some(explicit) = header(headers, CONVERSATION_HEADER)
        .or_else(|| header(headers, "x-session-id"))
        .or_else(|| header(headers, "x-session-affinity"))
    {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    implicit_conversation_id(body, model, effort)
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
        let conversation_id = conversation_id_from(headers, body, &model, effort);
        let next = fingerprint_body(body, &model, effort);
        let primer = sanitize_prompt(
            &flatten_prompt(body)
                .map_err(|message| ErrorBody::new(ErrorCode::InvalidConfig, message))?,
        );
        validate_prompt(&primer).map_err(pseudomux_service::v1::DriverFailure::into_protocol)?;

        let planned = {
            let mut guard = self.leases.lock().await;
            let expired_here = guard.live.get(&conversation_id).is_some_and(|lease| {
                !lease.in_flight && lease.last_used.elapsed() >= self.config.idle_ttl
            });
            if expired_here {
                // Hold reserved across the pool release so a concurrent prime
                // cannot bind a new cell that this release would then /clear.
                guard.live.remove(&conversation_id);
                guard.reserved.insert(conversation_id.clone());
                drop(guard);
                let _ = self.pool.release_conversation(&conversation_id).await;
                guard = self.leases.lock().await;
                guard.reserved.remove(&conversation_id);
            }
            let others = self.take_expired(&mut guard, Some(&conversation_id));
            drop(guard);
            for id in others {
                let _ = self.pool.release_conversation(&id).await;
                let mut g = self.leases.lock().await;
                g.reserved.remove(&id);
            }
            guard = self.leases.lock().await;
            if let Some(lease) = guard.live.get_mut(&conversation_id) {
                if lease.in_flight {
                    return Err(ErrorBody::new(
                        ErrorCode::SessionBusy,
                        "this conversation already has a turn in flight; nothing is queued",
                    )
                    .retryable(true));
                }
                let decision = classify_prefix(&lease.fingerprint, &next);
                if matches!(decision, PrefixDecision::Replay) {
                    lease.last_used = Instant::now();
                    return Ok(LeaseTurn {
                        conversation_id,
                        cell: lease.cell.clone(),
                        kind: LeaseKind::Replayed,
                        idle_ttl_ms: self.idle_ttl_ms(),
                        model,
                        result: lease.last_result.clone(),
                    });
                }
                lease.in_flight = true;
                Planned {
                    conversation_id: conversation_id.clone(),
                    model: model.clone(),
                    effort,
                    next: next.clone(),
                    kind: match decision {
                        PrefixDecision::Continue { from } => PlannedKind::Continue { from },
                        PrefixDecision::Reprime => PlannedKind::Reprime,
                        PrefixDecision::Replay => unreachable!(),
                    },
                }
            } else if guard.reserved.contains(&conversation_id) {
                return Err(ErrorBody::new(
                    ErrorCode::SessionBusy,
                    "this conversation already has a turn in flight; nothing is queued",
                )
                .retryable(true));
            } else {
                if u32::try_from(guard.occupied()).unwrap_or(u32::MAX) >= self.config.max_leases {
                    return Err(ErrorBody::new(
                        ErrorCode::SessionBusy,
                        format!(
                            "conversation lease cap is {}; release an idle conversation or raise --path-b-pool-size",
                            self.config.max_leases
                        ),
                    )
                    .retryable(true));
                }
                guard.reserved.insert(conversation_id.clone());
                Planned {
                    conversation_id: conversation_id.clone(),
                    model: model.clone(),
                    effort,
                    next,
                    kind: PlannedKind::Prime,
                }
            }
        };

        let outcome = self.execute(planned, body, &primer).await;
        if outcome.is_err() {
            let mut guard = self.leases.lock().await;
            guard.reserved.remove(&conversation_id);
            guard.live.remove(&conversation_id);
            drop(guard);
            // Pool bind may already be gone (quarantine) or still Leased
            // after a failed reprime's pre-release. Missing is success.
            let _ = self.pool.release_conversation(&conversation_id).await;
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
                return Err(ErrorBody::new(
                    ErrorCode::SessionBusy,
                    "this conversation already has a turn in flight; nothing is queued",
                )
                .retryable(true));
            }
            guard.live.remove(conversation_id);
        }
        self.pool.release_conversation(conversation_id).await
    }

    pub async fn sweep_expired(&self) {
        let mut guard = self.leases.lock().await;
        let expired = self.take_expired(&mut guard, None);
        drop(guard);
        for id in expired {
            let _ = self.pool.release_conversation(&id).await;
            let mut g = self.leases.lock().await;
            g.reserved.remove(&id);
        }
    }

    /// Remove expired leases from the book and mark them `reserved` so a
    /// concurrent prime cannot bind a new cell before `/clear` finishes.
    fn take_expired(&self, guard: &mut BookState, except: Option<&str>) -> Vec<String> {
        let ttl = self.config.idle_ttl;
        let expired: Vec<String> = guard
            .live
            .iter()
            .filter(|(id, lease)| {
                except != Some(id.as_str()) && !lease.in_flight && lease.last_used.elapsed() >= ttl
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            guard.live.remove(id);
            guard.reserved.insert(id.clone());
        }
        expired
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
            PlannedKind::Prime => (
                LeaseKind::Primed,
                self.pool
                    .run_sticky(&conversation_id, request_for(primer.to_owned()), false)
                    .await?,
            ),
            PlannedKind::Continue { from } => {
                let prompt = continuation_prompt(body, from)
                    .map_err(|message| ErrorBody::new(ErrorCode::InvalidConfig, message))?;
                validate_prompt(&prompt)
                    .map_err(pseudomux_service::v1::DriverFailure::into_protocol)?;
                match self
                    .pool
                    .run_sticky(&conversation_id, request_for(prompt), true)
                    .await
                {
                    Ok(turn) => (LeaseKind::Continued, turn),
                    Err(error) if error.code == ErrorCode::SessionNotFound => {
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
            model,
            result: turn.result,
        })
    }
}

struct Planned {
    conversation_id: String,
    model: String,
    effort: Option<EffortLevel>,
    next: ConversationFingerprint,
    kind: PlannedKind,
}

enum PlannedKind {
    Prime,
    Continue { from: usize },
    Reprime,
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
    hex(&hasher.finalize())
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
        );
        assert_eq!(id, "sess-1");
    }
}
