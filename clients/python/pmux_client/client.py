"""Pseudomux HTTP client."""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Optional

import urllib.request
import urllib.error
import json as _json


# ── Data classes ────────────────────────────────────────────────────────────


@dataclass
class ToolCall:
    name: Optional[str]
    duration_ms: Optional[int]


@dataclass
class PromptResult:
    session_id: str
    text: str
    duration_ms: int
    state: str
    tools: list[ToolCall] = field(default_factory=list)

    @classmethod
    def _from_json(cls, obj: dict) -> "PromptResult":
        return cls(
            session_id=obj["session_id"],
            text=obj["text"],
            duration_ms=obj["duration_ms"],
            state=obj["state"],
            tools=[
                ToolCall(name=t.get("name"), duration_ms=t.get("duration_ms"))
                for t in obj.get("tools", [])
            ],
        )


# ── Exceptions ──────────────────────────────────────────────────────────────


class PmuxError(Exception):
    """Base class for all pmux errors. Carries the optional session_id and
    the error code string from the daemon's typed error response."""

    def __init__(self, code: str, message: str, session_id: Optional[str] = None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.session_id = session_id


class TimeoutError(PmuxError):
    """Prompt did not complete within the allowed time."""


class AgentExitedError(PmuxError):
    """The agent subprocess exited while we were waiting for a response."""


class AuthRequiredError(PmuxError):
    """The agent is asking for authentication — caller must intervene."""


class ConfirmationRequiredError(PmuxError):
    """The agent is asking for a yes/no confirmation (tool permission, etc.)."""


class TransportError(PmuxError):
    """Daemon/HTTP-layer failure or unexpected response."""


_ERR_MAP = {
    "timeout": TimeoutError,
    "agent_exited": AgentExitedError,
    "auth_required": AuthRequiredError,
    "confirmation_required": ConfirmationRequiredError,
    "transport": TransportError,
}


def _raise_from(body: dict) -> None:
    error = body.get("error", "transport")
    if isinstance(error, dict):
        code = error.get("code", "transport")
        message = error.get("message", body.get("message", ""))
    else:
        code = error
        message = body.get("message", "")
    cls = _ERR_MAP.get(code, TransportError)
    raise cls(
        code=code,
        message=message,
        session_id=body.get("session_id"),
    )


# ── Client ──────────────────────────────────────────────────────────────────


class PmuxClient:
    """HTTP client for a running pmuxd.

    Start the daemon with `pmuxd serve --http-port 8765` and point this
    client at `http://localhost:8765`. Pass `token=` when pmuxd is started
    with `--http-token` or `PSEUDOMUX_HTTP_TOKEN`.
    """

    def __init__(self, base_url: str = "http://localhost:8765", token: Optional[str] = None):
        self.base_url = base_url.rstrip("/")
        self.token = token

    # ── Low-level HTTP helpers ─────────────────────────────────────────────

    def _request(
        self,
        method: str,
        path: str,
        body: Optional[dict] = None,
        timeout: Optional[float] = None,
    ) -> dict:
        url = f"{self.base_url}{path}"
        data = _json.dumps(body).encode() if body is not None else None
        headers = {"Content-Type": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        req = urllib.request.Request(
            url,
            data=data,
            method=method,
            headers=headers,
        )
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                return _json.loads(resp.read().decode())
        except urllib.error.HTTPError as e:
            try:
                err_body = _json.loads(e.read().decode())
            except Exception:
                raise TransportError("transport", f"HTTP {e.code}: {e.reason}") from e
            if "error" in err_body:
                _raise_from(err_body)
            raise TransportError("transport", f"HTTP {e.code}: {err_body}") from e
        except urllib.error.URLError as e:
            raise TransportError("transport", f"connection failed: {e}") from e

    # ── High-level API ─────────────────────────────────────────────────────

    def health(self) -> dict:
        """Return the daemon's health status."""
        return self._request("GET", "/health")

    def run(
        self,
        text: str,
        *,
        agent: str = "claude-code",
        cwd: Optional[str] = None,
        name: Optional[str] = None,
        args: Optional[list[str]] = None,
        timeout_secs: int = 120,
        keep_alive: bool = False,
    ) -> PromptResult:
        """One-shot: start a session, send a prompt, return the result.

        Unless `keep_alive=True`, the session is terminated automatically.
        """
        body: dict = {
            "text": text,
            "timeout_secs": timeout_secs,
            "keep_alive": keep_alive,
            "session": {
                "agent": agent,
                "args": args or [],
                "env": [],
                "cwd": cwd,
                "name": name,
            },
        }
        out = self._request("POST", "/run", body, timeout=timeout_secs + 30)
        return PromptResult._from_json(out)

    def start_session(
        self,
        *,
        agent: str = "claude-code",
        cwd: Optional[str] = None,
        name: Optional[str] = None,
        args: Optional[list[str]] = None,
    ) -> str:
        """Create a persistent session and return its UUID. Use `prompt()`
        for follow-up turns and `stop_session()` when done."""
        body = {
            "agent": agent,
            "args": args or [],
            "env": [],
            "cwd": cwd,
            "name": name,
        }
        out = self._request("POST", "/sessions", body)
        return out["session"]

    def wait_ready(self, session_id: str, timeout_secs: int = 30) -> None:
        """Block until the session's agent reaches Ready state. Polls every
        500ms. Raises `TimeoutError` if the agent doesn't boot in time."""
        deadline = time.monotonic() + timeout_secs
        while True:
            out = self._request("GET", f"/sessions/{session_id}/state")
            if out.get("state") == "Ready":
                return
            if time.monotonic() >= deadline:
                raise TimeoutError(
                    "timeout",
                    f"session {session_id} did not reach Ready in {timeout_secs}s",
                    session_id,
                )
            time.sleep(0.5)

    def prompt(
        self,
        session_id: str,
        text: str,
        *,
        timeout_secs: int = 120,
    ) -> PromptResult:
        """Send a prompt on an existing session, block until TurnComplete,
        and return the result. The session stays alive."""
        body = {"text": text, "timeout_secs": timeout_secs}
        out = self._request(
            "POST",
            f"/sessions/{session_id}/prompt-sync",
            body,
            timeout=timeout_secs + 30,
        )
        return PromptResult._from_json(out)

    def stop_session(self, session_id: str) -> None:
        """Terminate a session."""
        self._request("DELETE", f"/sessions/{session_id}")

    def list_sessions(self) -> list[dict]:
        """Return a list of all active sessions."""
        return self._request("GET", "/sessions").get("sessions", [])
