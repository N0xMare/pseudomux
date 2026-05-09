use anyhow::{Result, bail};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use pseudomux_protocol::{
    ContentSinceParams, ReadSinceParams, Request, Response, SessionStateParams,
    SubscribeEventsParams,
};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::StateFormat;
use crate::client::{DaemonClient, connect_to_daemon, resolve_session};

pub(crate) async fn handle_read(client: &DaemonClient, session: &str, since: u64) -> Result<()> {
    let session = resolve_session(client, session).await?;
    let resp = client
        .send(Request::ReadSince(ReadSinceParams {
            session,
            seq: since,
        }))
        .await?;
    match resp {
        Response::Output { chunks, next_seq } => {
            for chunk in chunks {
                print!("{}", String::from_utf8_lossy(&chunk.bytes));
            }
            eprintln!("\n-- next_seq: {next_seq}");
        }
        Response::Error { code, message } => bail!("{code}: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
    Ok(())
}

pub(crate) async fn handle_content(
    client: &DaemonClient,
    session: &str,
    since_seq: Option<u64>,
    last: Option<usize>,
    raw: bool,
    json: bool,
) -> Result<()> {
    let session = resolve_session(client, session).await?;
    if raw {
        let (entries, next_seq) = if let Some(seq) = since_seq {
            let resp = client
                .send(Request::GetContentSince(ContentSinceParams {
                    session,
                    seq,
                }))
                .await?;
            match resp {
                Response::Content { entries, next_seq } => (entries, next_seq),
                Response::Error { code, message } => bail!("{code}: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        } else {
            let resp = client
                .send(Request::GetContentSinceLastInput(SessionStateParams {
                    session,
                }))
                .await?;
            match resp {
                Response::Content { entries, next_seq } => (entries, next_seq),
                Response::Error { code, message } => bail!("{code}: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        };
        let entries = if let Some(n) = last {
            let skip = entries.len().saturating_sub(n);
            entries[skip..].to_vec()
        } else {
            entries
        };
        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "entries": entries,
                    "next_seq": next_seq,
                }))?
            );
        } else {
            for entry in &entries {
                print!("{}", entry.text);
            }
        }
    } else {
        let (text, next_seq) = if let Some(seq) = since_seq {
            let resp = client
                .send(Request::GetFilteredContent(ContentSinceParams {
                    session,
                    seq,
                }))
                .await?;
            match resp {
                Response::FilteredContent { text, next_seq } => (text, next_seq),
                Response::Error { code, message } => bail!("{code}: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        } else {
            let resp = client
                .send(Request::GetFilteredContentSinceLastInput(
                    SessionStateParams { session },
                ))
                .await?;
            match resp {
                Response::FilteredContent { text, next_seq } => (text, next_seq),
                Response::Error { code, message } => bail!("{code}: {message}"),
                other => bail!("unexpected response: {other:?}"),
            }
        };
        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({"text": text, "next_seq": next_seq}))?
            );
        } else {
            print!("{text}");
        }
    }
    Ok(())
}

pub(crate) async fn handle_content_seq(client: &DaemonClient, session: &str) -> Result<()> {
    let session = resolve_session(client, session).await?;
    let resp = client
        .send(Request::GetContentCurrentSeq(SessionStateParams {
            session,
        }))
        .await?;
    match resp {
        Response::ContentSeq { seq } => println!("{seq}"),
        Response::Error { code, message } => bail!("{code}: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
    Ok(())
}

pub(crate) async fn handle_screen_text(
    client: &DaemonClient,
    session: &str,
    status: bool,
) -> Result<()> {
    let session = resolve_session(client, session).await?;
    if status {
        let resp = client
            .send(Request::GetStatusText(SessionStateParams { session }))
            .await?;
        match resp {
            Response::StatusText { text } => println!("{text}"),
            Response::Error { code, message } => bail!("{code}: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    } else {
        let resp = client
            .send(Request::GetContentText(SessionStateParams { session }))
            .await?;
        match resp {
            Response::ContentText { text } => println!("{text}"),
            Response::Error { code, message } => bail!("{code}: {message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }
    Ok(())
}

pub(crate) async fn handle_agent_state(
    client: &DaemonClient,
    session: &str,
    format: StateFormat,
) -> Result<()> {
    let session = resolve_session(client, session).await?;
    let resp = client
        .send(Request::GetAgentState(SessionStateParams { session }))
        .await?;
    match resp {
        Response::AgentState { state } => match format {
            StateFormat::Text => println!("agent_state session={session} state={state}"),
            StateFormat::Json => println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "type": "agent_state",
                    "session": session,
                    "state": state,
                    "source": "vte",
                }))?
            ),
        },
        Response::Error { code, message } => bail!("{code}: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
    Ok(())
}

pub(crate) async fn handle_terminal_state(
    client: &DaemonClient,
    session: &str,
    format: StateFormat,
) -> Result<()> {
    let session = resolve_session(client, session).await?;
    let resp = client
        .send(Request::GetTerminalState(SessionStateParams { session }))
        .await?;
    match resp {
        Response::TerminalState {
            keyboard_mode,
            bracketed_paste,
            focus_events,
        } => match format {
            StateFormat::Text => {
                println!(
                    "terminal_state session={session} keyboard_mode={keyboard_mode} bracketed_paste={bracketed_paste} focus_events={focus_events}"
                );
            }
            StateFormat::Json => {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "type": "terminal_state",
                        "session": session,
                        "keyboard_mode": keyboard_mode,
                        "bracketed_paste": bracketed_paste,
                        "focus_events": focus_events,
                    }))?
                );
            }
        },
        Response::Error { code, message } => bail!("{code}: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
    Ok(())
}

pub(crate) async fn handle_events(
    client: &DaemonClient,
    session: &str,
    timeout_ms: u64,
    max_events: u64,
) -> Result<()> {
    let session = resolve_session(client, session).await?;
    let request = Request::SubscribeEvents(SubscribeEventsParams {
        session,
        timeout_ms,
        max_events,
    });
    let payload = serde_json::to_vec(&request)?;
    let stream = connect_to_daemon(&client.sockets).await?;
    let mut framed = Framed::new(stream, LengthDelimitedCodec::new());
    framed.send(Bytes::from(payload)).await?;
    while let Some(frame) = framed.next().await {
        let bytes = frame?;
        let response: Response = serde_json::from_slice(&bytes)?;
        match response {
            Response::SemanticEvent { event } => {
                println!("{}", serde_json::to_string(&event)?);
            }
            Response::Error { code, message } => bail!("{code}: {message}"),
            _ => break,
        }
    }
    Ok(())
}

pub(crate) async fn handle_watch(
    client: &DaemonClient,
    session: &str,
    json: bool,
    timeout_ms: u64,
    max_events: u64,
) -> Result<()> {
    let session = resolve_session(client, session).await?;
    let request = Request::SubscribeWatchEvents(SubscribeEventsParams {
        session,
        timeout_ms,
        max_events,
    });
    let payload = serde_json::to_vec(&request)?;
    let stream = connect_to_daemon(&client.sockets).await?;
    let mut framed = Framed::new(stream, LengthDelimitedCodec::new());
    framed.send(Bytes::from(payload)).await?;
    while let Some(frame) = framed.next().await {
        let bytes = frame?;
        let response: Response = serde_json::from_slice(&bytes)?;
        match response {
            Response::WatchEvent { event } => {
                if json {
                    println!("{}", serde_json::to_string(&event)?);
                } else if let Some(obj) = event.as_object() {
                    let kind = obj.keys().next().unwrap_or(&String::new()).clone();
                    println!("[watch] {kind}: {event}");
                } else {
                    println!("[watch] {event}");
                }
            }
            Response::Error { code, message } => bail!("{code}: {message}"),
            _ => break,
        }
    }
    Ok(())
}
