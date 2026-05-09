"""Thin Python client for the pmuxd HTTP API.

Typical usage:

    from pmux_client import PmuxClient

    client = PmuxClient("http://localhost:8765")

    # Mode 1: one-shot
    result = client.run(
        text="Review this repo",
        agent="claude-code",
        cwd="/path/to/repo",
        args=["--model", "opus", "--permission-mode", "bypassPermissions"],
    )
    print(result.text)

    # Mode 2: persistent orchestrator
    session = client.start_session(
        agent="claude-code",
        cwd="/path/to/repo",
        args=["--model", "opus", "--permission-mode", "bypassPermissions"],
    )
    client.wait_ready(session)
    result1 = client.prompt(session, "Read main.rs and summarize")
    result2 = client.prompt(session, "Now propose 3 improvements")
    client.stop_session(session)
"""

from .client import (
    PmuxClient,
    PromptResult,
    ToolCall,
    PmuxError,
    TimeoutError,
    AgentExitedError,
    AuthRequiredError,
    ConfirmationRequiredError,
    TransportError,
)

__all__ = [
    "PmuxClient",
    "PromptResult",
    "ToolCall",
    "PmuxError",
    "TimeoutError",
    "AgentExitedError",
    "AuthRequiredError",
    "ConfirmationRequiredError",
    "TransportError",
]
__version__ = "0.1.0"
