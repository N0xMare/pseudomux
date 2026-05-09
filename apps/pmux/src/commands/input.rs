use anyhow::Result;
use pseudomux_protocol::{
    Request, SendActionParams, SendKeyParams, SendPromptParams, SendTextParams,
};

use crate::client::{DaemonClient, expect_ack, resolve_session};

pub(crate) async fn handle_send(client: &DaemonClient, session: &str, text: String) -> Result<()> {
    let session = resolve_session(client, session).await?;
    expect_ack(
        client
            .send(Request::SendText(SendTextParams { session, text }))
            .await?,
    )
}

pub(crate) async fn handle_input_key(
    client: &DaemonClient,
    session: &str,
    key: String,
) -> Result<()> {
    let session = resolve_session(client, session).await?;
    expect_ack(
        client
            .send(Request::SendKey(SendKeyParams { session, key }))
            .await?,
    )
}

pub(crate) async fn handle_input_action(
    client: &DaemonClient,
    session: &str,
    action: String,
) -> Result<()> {
    let session = resolve_session(client, session).await?;
    expect_ack(
        client
            .send(Request::SendAction(SendActionParams { session, action }))
            .await?,
    )
}

pub(crate) async fn handle_input_prompt(
    client: &DaemonClient,
    session: &str,
    text: String,
) -> Result<()> {
    let session = resolve_session(client, session).await?;
    expect_ack(
        client
            .send(Request::SendPrompt(SendPromptParams { session, text }))
            .await?,
    )
}

pub(crate) async fn handle_confirm(client: &DaemonClient, session: &str, no: bool) -> Result<()> {
    let session = resolve_session(client, session).await?;
    let action = if no { "confirm_no" } else { "confirm_yes" };
    expect_ack(
        client
            .send(Request::SendAction(SendActionParams {
                session,
                action: action.to_string(),
            }))
            .await?,
    )
}
