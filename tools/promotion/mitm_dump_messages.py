"""mitmproxy addon: write each Anthropic /v1/messages request body to dump_dir.

Auth headers are never written. Loaded by `mitmdump -s` with
`PMUX_MITM_DUMP_DIR` set.
"""

from __future__ import annotations

import json
import os
import pathlib
import time

from mitmproxy import http

STRIP_HEADERS = frozenset(
    {
        "authorization",
        "proxy-authorization",
        "x-api-key",
        "anthropic-api-key",
        "cookie",
        "set-cookie",
        "x-session-id",
    }
)


def dump_dir() -> pathlib.Path:
    raw = os.environ.get("PMUX_MITM_DUMP_DIR")
    if not raw:
        raise RuntimeError("PMUX_MITM_DUMP_DIR is unset")
    path = pathlib.Path(raw)
    path.mkdir(parents=True, exist_ok=True)
    return path


class DumpMessages:
    def __init__(self) -> None:
        self.n = 0

    def request(self, flow: http.HTTPFlow) -> None:
        host = (flow.request.pretty_host or flow.request.host or "").lower()
        path = flow.request.path or ""
        if "anthropic.com" not in host and "anthropic" not in host:
            return
        if "/v1/messages" not in path:
            return
        self.n += 1
        headers = {
            name: value
            for name, value in flow.request.headers.items()
            if name.lower() not in STRIP_HEADERS
        }
        raw = flow.request.content or b""
        text = raw.decode("utf-8", errors="replace")
        try:
            body = json.loads(text) if text else None
        except json.JSONDecodeError:
            body = None
        record = {
            "n": self.n,
            "t_monotonic": time.monotonic(),
            "host": flow.request.pretty_host,
            "path": path.split("?", 1)[0],
            "method": flow.request.method,
            "content_type": flow.request.headers.get("content-type"),
            "bytes": len(raw),
            "headers": headers,
            "body": body,
            "body_parse_ok": body is not None,
        }
        if body is None:
            record["body_preview"] = text[:400]
        dest = dump_dir() / f"{self.n:03d}-messages.json"
        dest.write_text(json.dumps(record, indent=1, sort_keys=True) + "\n", encoding="utf-8")
        dest.chmod(0o600)


addons = [DumpMessages()]
