"""Loopback Messages listener helpers. Harnesses POST /v1/messages themselves."""

from __future__ import annotations

import ipaddress
import json
import time
from collections.abc import MutableMapping
from http.client import HTTPConnection, HTTPException, HTTPResponse
from typing import Any, Final
from urllib.parse import urlparse

PMUX_CONVERSATION_HEADER: Final = "x-pmux-conversation"
PMUX_CONVERSATION_HEADER_ALIASES: Final = (
    "x-pmux-conversation",
    "x-session-id",
    "x-session-affinity",
)
_DEFAULT_TIMEOUT: Final = 10.0
MAX_RESPONSE_BYTES: Final = 2 * 1024 * 1024
_READ_CHUNK: Final = 8192
# Exact host names; other 127/8 addresses such as 127.0.0.2 are refused.
_LOOPBACK_HOSTS: Final = frozenset({"127.0.0.1", "localhost", "::1"})


class PmuxMessagesError(Exception):
    """Messages listener HTTP helper failure."""

    def __init__(self, message: str, *, status: int = 0, body: str = "") -> None:
        super().__init__(message)
        self.status = status
        self.body = body


def _path_safe_conversation_id(conversation_id: str) -> str:
    identity = conversation_id.strip()
    if not identity:
        raise ValueError("conversation id must not be empty")
    if any(ch in "/?#" or ch.isspace() for ch in identity):
        raise ValueError("conversation id is not path-safe")
    return identity


def conversation_header(conversation_id: str) -> tuple[str, str]:
    return (PMUX_CONVERSATION_HEADER, _path_safe_conversation_id(conversation_id))


def set_conversation_header(headers: MutableMapping[str, str], conversation_id: str) -> None:
    name, value = conversation_header(conversation_id)
    headers[name] = value


def _parse_loopback_http_url(base_url: str) -> tuple[str, str, int]:
    raw = base_url.strip().rstrip("/")
    if not raw:
        raise ValueError("base_url must not be empty")
    parsed = urlparse(raw)
    if parsed.scheme != "http" or parsed.username is not None or parsed.password is not None:
        raise ValueError("Messages URL must be http://HOST:PORT")
    try:
        host = parsed.hostname
        port = parsed.port
    except ValueError as error:
        raise ValueError("Messages URL must be http://HOST:PORT") from error
    if host is None or port is None:
        raise ValueError("Messages URL must be http://HOST:PORT")
    host_key = host.lower()
    if host_key not in _LOOPBACK_HOSTS:
        try:
            ipaddress.ip_address(host)
        except ValueError:
            raise ValueError(f"cannot parse {host}") from None
        raise ValueError("Messages client is loopback-only")
    if host_key == "::1":
        return (f"http://[::1]:{port}", "::1", port)
    return (f"http://{host_key}:{port}", host_key, port)


def _remaining(deadline: float) -> float:
    left = deadline - time.monotonic()
    if left <= 0:
        raise TimeoutError("Messages HTTP timed out")
    return left


class PmuxMessages:
    """Loopback-only helper for pin, release, and catalog. Does not POST /v1/messages."""

    def __init__(
        self,
        base_url: str,
        *,
        api_key: str | None = None,
        timeout: float = _DEFAULT_TIMEOUT,
    ) -> None:
        origin, host, port = _parse_loopback_http_url(base_url)
        key = (api_key if api_key is not None else "pmux").strip() or "pmux"
        if any(ch in key for ch in ("\r", "\n", "\0")):
            raise ValueError("api_key contains CR, LF, or NUL")
        if timeout <= 0:
            raise ValueError("timeout must be greater than zero")
        self.base_url = origin
        self.api_key = key
        self._host = host
        self._port = port
        self._timeout = timeout

    def release(self, conversation_id: str) -> None:
        identity = _path_safe_conversation_id(conversation_id)
        path = f"/v1/conversations/{identity}/release"
        self._exchange("POST", path)

    def models(self) -> Any:
        return self._get_json("/v1/models")

    def capabilities(self) -> Any:
        return self._get_json("/v1/capabilities")

    def _auth_headers(self) -> dict[str, str]:
        return {
            "x-api-key": self.api_key,
            "Authorization": f"Bearer {self.api_key}",
        }

    def _get_json(self, path: str) -> Any:
        _status, body = self._exchange("GET", path)
        try:
            return json.loads(body)
        except json.JSONDecodeError as error:
            raise PmuxMessagesError(f"pmux {path} returned invalid JSON", body=body) from error

    def _apply_timeout(self, connection: HTTPConnection, deadline: float) -> None:
        remaining = _remaining(deadline)
        connection.timeout = remaining
        if connection.sock is not None:
            connection.sock.settimeout(remaining)

    def _read_capped(
        self,
        response: HTTPResponse,
        connection: HTTPConnection,
        deadline: float,
    ) -> bytes:
        chunks: list[bytes] = []
        total = 0
        while True:
            self._apply_timeout(connection, deadline)
            chunk = response.read(min(_READ_CHUNK, MAX_RESPONSE_BYTES - total + 1))
            if not chunk:
                break
            total += len(chunk)
            if total > MAX_RESPONSE_BYTES:
                raise OSError(f"Messages HTTP response exceeds {MAX_RESPONSE_BYTES} bytes")
            chunks.append(chunk)
        return b"".join(chunks)

    def _exchange(self, method: str, path: str) -> tuple[int, str]:
        deadline = time.monotonic() + self._timeout
        connection = HTTPConnection(self._host, self._port, timeout=_remaining(deadline))
        try:
            connection.connect()
            self._apply_timeout(connection, deadline)
            connection.request(method, path, headers=self._auth_headers())
            self._apply_timeout(connection, deadline)
            response = connection.getresponse()
            body_bytes = self._read_capped(response, connection, deadline)
            body = body_bytes.decode("utf-8", errors="replace")
            status = response.status
        except TimeoutError as error:
            raise PmuxMessagesError(f"pmux {path} failed: Messages HTTP timed out") from error
        except (HTTPException, OSError) as error:
            raise PmuxMessagesError(f"pmux {path} failed: {error}") from error
        finally:
            connection.close()
        if status != 200:
            label = "release" if method == "POST" else path
            raise PmuxMessagesError(f"pmux {label} {status}: {body}", status=status, body=body)
        return (status, body)
