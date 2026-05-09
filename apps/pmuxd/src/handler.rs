use crate::session::start_session;
use crate::util::{entry_to_dto, summarize};
use anyhow::Result;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use pseudomux_core::session::state::TerminalSize;
use pseudomux_protocol::{Request, Response, SessionId};
use pseudomux_service::Service;
use std::sync::Arc;
use tokio::net::UnixStream;
use tokio::sync::broadcast;
use tokio::time::Duration;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::warn;

pub(crate) async fn handle_client(stream: UnixStream, service: Arc<Service>) -> Result<()> {
    let mut framed = Framed::new(stream, LengthDelimitedCodec::new());
    while let Some(frame) = framed.next().await {
        let bytes = frame?;
        let request: Request = match serde_json::from_slice(&bytes) {
            Ok(req) => req,
            Err(err) => {
                let payload = serde_json::to_vec(&Response::error("bad_request", err.to_string()))?;
                framed.send(Bytes::from(payload)).await?;
                continue;
            }
        };
        if let Request::SubscribeEvents(ref params) = request {
            handle_subscribe_events(&mut framed, Arc::clone(&service), params.clone()).await?;
            continue;
        }
        if let Request::SubscribeWatchEvents(ref params) = request {
            handle_subscribe_watch_events(&mut framed, Arc::clone(&service), params.clone())
                .await?;
            continue;
        }
        let response = handle_request(Arc::clone(&service), request).await;
        let payload = serde_json::to_vec(&response)?;
        framed.send(Bytes::from(payload)).await?;
    }
    Ok(())
}

async fn stream_broadcast_events<T, F>(
    framed: &mut Framed<UnixStream, LengthDelimitedCodec>,
    service: Arc<Service>,
    params: &pseudomux_protocol::SubscribeEventsParams,
    subscribe_fn: F,
    wrap_response: fn(serde_json::Value) -> Response,
    event_kind: &str,
) -> Result<()>
where
    T: Clone + Send + serde::Serialize + 'static,
    F: FnOnce(Arc<Service>, SessionId) -> anyhow::Result<broadcast::Receiver<T>> + Send + 'static,
{
    let session = params.session;
    let mut rx = match tokio::task::spawn_blocking(move || subscribe_fn(service, session)).await {
        Ok(Ok(rx)) => rx,
        Ok(Err(err)) => {
            let payload =
                serde_json::to_vec(&Response::error("subscribe_failed", err.to_string()))?;
            framed.send(Bytes::from(payload)).await?;
            return Ok(());
        }
        Err(err) => {
            let payload = serde_json::to_vec(&Response::error("task_failed", err.to_string()))?;
            framed.send(Bytes::from(payload)).await?;
            return Ok(());
        }
    };

    let deadline = if params.timeout_ms > 0 {
        tokio::time::Instant::now() + Duration::from_millis(params.timeout_ms)
    } else {
        tokio::time::Instant::now() + Duration::from_secs(30)
    };
    let max_events = if params.max_events > 0 {
        params.max_events
    } else {
        u64::MAX
    };
    let mut count: u64 = 0;

    loop {
        if count >= max_events {
            break;
        }
        let Ok(recv_result) = tokio::time::timeout_at(deadline, rx.recv()).await else {
            break;
        };
        match recv_result {
            Ok(event) => {
                let Ok(val) = serde_json::to_value(&event) else {
                    continue;
                };
                let payload = serde_json::to_vec(&wrap_response(val))?;
                if framed.send(Bytes::from(payload)).await.is_err() {
                    break;
                }
                count += 1;
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(session = %session, lagged = n, "{} subscriber lagged", event_kind);
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }

    let payload = serde_json::to_vec(&Response::ok())?;
    let _ = framed.send(Bytes::from(payload)).await;
    Ok(())
}

async fn handle_subscribe_events(
    framed: &mut Framed<UnixStream, LengthDelimitedCodec>,
    service: Arc<Service>,
    params: pseudomux_protocol::SubscribeEventsParams,
) -> Result<()> {
    stream_broadcast_events(
        framed,
        service,
        &params,
        |svc, session| svc.subscribe_events(session),
        |event| Response::SemanticEvent { event },
        "event",
    )
    .await
}

async fn handle_subscribe_watch_events(
    framed: &mut Framed<UnixStream, LengthDelimitedCodec>,
    service: Arc<Service>,
    params: pseudomux_protocol::SubscribeEventsParams,
) -> Result<()> {
    stream_broadcast_events(
        framed,
        service,
        &params,
        |svc, session| svc.subscribe_watch_events(session),
        |event| Response::WatchEvent { event },
        "watch event",
    )
    .await
}

async fn call_blocking<T, F>(service: Arc<Service>, f: F) -> Result<T, Response>
where
    T: Send + 'static,
    F: FnOnce(Arc<Service>) -> anyhow::Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(move || f(service)).await {
        Ok(Ok(val)) => Ok(val),
        Ok(Err(err)) => Err(Response::error("operation_failed", err.to_string())),
        Err(err) => Err(Response::error("task_failed", err.to_string())),
    }
}

async fn call_unit<F>(service: Arc<Service>, f: F) -> Response
where
    F: FnOnce(Arc<Service>) -> anyhow::Result<()> + Send + 'static,
{
    match call_blocking(service, f).await {
        Ok(()) => Response::ok(),
        Err(resp) => resp,
    }
}

pub(crate) async fn handle_request(service: Arc<Service>, request: Request) -> Response {
    match request {
        Request::Ping => Response::Pong,
        Request::StartSession(params) => {
            let svc = Arc::clone(&service);
            match tokio::task::spawn_blocking(move || start_session(&svc, params)).await {
                Ok(Ok(session)) => Response::StartSession { session },
                Ok(Err(err)) => Response::error("start_failed", err.to_string()),
                Err(err) => Response::error("task_failed", err.to_string()),
            }
        }
        Request::SendText(params) => {
            let session = params.session;
            let text = params.text;
            call_unit(service, move |svc| svc.send_text(session, &text)).await
        }
        Request::SendBytes(params) => {
            let session = params.session;
            let bytes = params.bytes;
            call_unit(service, move |svc| svc.send_bytes(session, &bytes)).await
        }
        Request::SendEnter(params) => {
            let session = params.session;
            call_unit(service, move |svc| svc.send_enter(session)).await
        }
        Request::ReadSince(params) => {
            let session = params.session;
            let seq = params.seq;
            match call_blocking(service, move |svc| svc.read_since(session, seq)).await {
                Ok((chunks, next_seq)) => Response::Output { chunks, next_seq },
                Err(resp) => resp,
            }
        }
        Request::Resize(params) => {
            let session = params.session;
            let rows = params.rows;
            let cols = params.cols;
            call_unit(service, move |svc| {
                svc.resize(session, TerminalSize { rows, cols })
            })
            .await
        }
        Request::Interrupt(params) => {
            let session = params.session;
            call_unit(service, move |svc| svc.interrupt(session)).await
        }
        Request::Terminate(params) => {
            let session = params.session;
            call_unit(service, move |svc| svc.terminate(session)).await
        }
        Request::GetState(params) => {
            let session = params.session;
            match call_blocking(service, move |svc| svc.state(session)).await {
                Ok(info) => Response::SessionState {
                    summary: summarize(info),
                },
                Err(resp) => resp,
            }
        }
        Request::GetAgentState(params) => {
            let session = params.session;
            match call_blocking(service, move |svc| svc.agent_state(session)).await {
                Ok(state) => Response::AgentState {
                    state: format!("{state:?}"),
                },
                Err(resp) => resp,
            }
        }
        Request::GetContentText(params) => {
            let session = params.session;
            match call_blocking(service, move |svc| svc.content_text(session)).await {
                Ok(text) => Response::ContentText { text },
                Err(resp) => resp,
            }
        }
        Request::GetStatusText(params) => {
            let session = params.session;
            match call_blocking(service, move |svc| svc.status_text(session)).await {
                Ok(text) => Response::StatusText { text },
                Err(resp) => resp,
            }
        }
        Request::SendKey(params) => {
            let session = params.session;
            let key_name = params.key;
            match call_blocking(service, move |svc| {
                let key = pseudomux_core::input::parse_key_name(&key_name)
                    .map_err(|e| anyhow::anyhow!("invalid key name: {e}"))?;
                svc.send_key(session, key)
            })
            .await
            {
                Ok(()) => Response::ok(),
                Err(resp) => resp,
            }
        }
        Request::SendAction(params) => {
            let session = params.session;
            let action = params.action;
            call_unit(service, move |svc| svc.send_action(session, &action)).await
        }
        Request::SendPrompt(params) => {
            let session = params.session;
            let text = params.text;
            call_unit(service, move |svc| svc.send_prompt(session, &text)).await
        }
        Request::GetTerminalState(params) => {
            let session = params.session;
            match call_blocking(service, move |svc| svc.terminal_state(session)).await {
                Ok(state) => Response::TerminalState {
                    keyboard_mode: format!("{:?}", state.keyboard_mode),
                    bracketed_paste: state.bracketed_paste,
                    focus_events: state.focus_events,
                },
                Err(resp) => resp,
            }
        }
        Request::GetContentSince(params) => {
            let session = params.session;
            let seq = params.seq;
            match call_blocking(service, move |svc| svc.content_since_seq(session, seq)).await {
                Ok(entries) => {
                    let next_seq = entries.last().map_or(seq, |e| e.seq + 1);
                    Response::Content {
                        entries: entries.iter().map(entry_to_dto).collect(),
                        next_seq,
                    }
                }
                Err(resp) => resp,
            }
        }
        Request::GetContentSinceLastInput(params) => {
            let session = params.session;
            match call_blocking(service, move |svc| svc.content_since_last_input(session)).await {
                Ok(entries) => {
                    let next_seq = entries.last().map_or(0, |e| e.seq + 1);
                    Response::Content {
                        entries: entries.iter().map(entry_to_dto).collect(),
                        next_seq,
                    }
                }
                Err(resp) => resp,
            }
        }
        Request::GetContentCurrentSeq(params) => {
            let session = params.session;
            match call_blocking(service, move |svc| svc.content_current_seq(session)).await {
                Ok(seq) => Response::ContentSeq { seq },
                Err(resp) => resp,
            }
        }
        Request::GetFilteredContent(params) => {
            let session = params.session;
            let seq = params.seq;
            match call_blocking(service, move |svc| {
                let text = svc.filtered_content_since_seq(session, seq)?;
                let current_seq = svc.content_current_seq(session)?;
                Ok::<_, anyhow::Error>((text, current_seq))
            })
            .await
            {
                Ok((text, next_seq)) => Response::FilteredContent { text, next_seq },
                Err(resp) => resp,
            }
        }
        Request::GetFilteredContentSinceLastInput(params) => {
            let session = params.session;
            match call_blocking(service, move |svc| {
                let text = svc.filtered_content_since_last_input(session)?;
                let current_seq = svc.content_current_seq(session)?;
                Ok::<_, anyhow::Error>((text, current_seq))
            })
            .await
            {
                Ok((text, next_seq)) => Response::FilteredContent { text, next_seq },
                Err(resp) => resp,
            }
        }
        Request::GetFilteredScreenContent(params) => {
            let session = params.session;
            match call_blocking(service, move |svc| {
                let text = svc.filtered_screen_content(session)?;
                let current_seq = svc.content_current_seq(session)?;
                Ok::<_, anyhow::Error>((text, current_seq))
            })
            .await
            {
                Ok((text, next_seq)) => Response::FilteredContent { text, next_seq },
                Err(resp) => resp,
            }
        }
        Request::GetFilteredResponseSinceLastInput(params) => {
            let session = params.session;
            match call_blocking(service, move |svc| {
                let text = svc.filtered_response_since_last_input(session)?;
                let current_seq = svc.content_current_seq(session)?;
                Ok::<_, anyhow::Error>((text, current_seq))
            })
            .await
            {
                Ok((text, next_seq)) => Response::FilteredContent { text, next_seq },
                Err(resp) => resp,
            }
        }
        Request::SubscribeWatchEvents(_) => Response::error(
            "internal",
            "SubscribeWatchEvents handled at connection level",
        ),
        Request::SubscribeEvents(_) => {
            Response::error("internal", "SubscribeEvents handled at connection level")
        }
        Request::ListSessions => {
            let svc = Arc::clone(&service);
            match tokio::task::spawn_blocking(move || svc.list_sessions()).await {
                Ok(sessions) => Response::Sessions {
                    sessions: sessions.into_iter().map(summarize).collect(),
                },
                Err(err) => Response::error("task_failed", err.to_string()),
            }
        }
    }
}
