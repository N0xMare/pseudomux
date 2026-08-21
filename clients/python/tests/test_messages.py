from __future__ import annotations

import contextlib
import json
import socket
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

from pmux_client import (
    PMUX_CONVERSATION_HEADER,
    PMUX_CONVERSATION_HEADER_ALIASES,
    PmuxMessages,
    PmuxMessagesError,
    conversation_header,
    set_conversation_header,
)


class _RecordingHandler(BaseHTTPRequestHandler):
    def log_message(self, format: str, *args: object) -> None:
        del format, args

    def do_GET(self) -> None:
        self._record_and_reply()

    def do_POST(self) -> None:
        self._record_and_reply()

    def _record_and_reply(self) -> None:
        server = self.server
        assert isinstance(server, _RecordingServer)
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        server.seen.append(
            {
                "method": self.command,
                "path": self.path,
                "x-api-key": self.headers.get("x-api-key"),
                "authorization": self.headers.get("authorization"),
            }
        )
        payload = json.dumps({"ok": True, "path": self.path}).encode()
        self.send_response(server.status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(payload)


class _RecordingServer(ThreadingHTTPServer):
    def __init__(self, status: int = 200) -> None:
        super().__init__(("127.0.0.1", 0), _RecordingHandler)
        self.seen: list[dict[str, Any]] = []
        self.status = status
        self._thread = threading.Thread(target=self.serve_forever, daemon=True)

    def __enter__(self) -> _RecordingServer:
        self._thread.start()
        return self

    def __exit__(self, exc_type: object, exc: object, traceback: object) -> None:
        self.shutdown()
        self.server_close()
        self._thread.join(timeout=4)
        if self._thread.is_alive() and exc is None:
            raise RuntimeError("messages fixture server did not finish")


class MessagesHelperTests(unittest.TestCase):
    def test_set_conversation_header_writes_the_pin(self) -> None:
        headers: dict[str, str] = {}
        set_conversation_header(headers, " sess-1 ")
        self.assertEqual(headers[PMUX_CONVERSATION_HEADER], "sess-1")
        self.assertEqual(conversation_header(" abc "), (PMUX_CONVERSATION_HEADER, "abc"))
        self.assertEqual(
            PMUX_CONVERSATION_HEADER_ALIASES,
            ("x-pmux-conversation", "x-session-id", "x-session-affinity"),
        )

    def test_empty_conversation_id_is_rejected(self) -> None:
        headers: dict[str, str] = {}
        with self.assertRaisesRegex(ValueError, "conversation id must not be empty"):
            set_conversation_header(headers, "  ")
        with self.assertRaisesRegex(ValueError, "conversation id must not be empty"):
            conversation_header("")
        with self.assertRaisesRegex(ValueError, "conversation id must not be empty"):
            PmuxMessages("http://127.0.0.1:8765").release("")
        with self.assertRaisesRegex(ValueError, "conversation id must not be empty"):
            PmuxMessages("http://127.0.0.1:8765").release(" \t ")
        self.assertEqual(headers, {})

    def test_path_unsafe_conversation_ids_are_rejected(self) -> None:
        headers: dict[str, str] = {}
        client = PmuxMessages("http://127.0.0.1:8765")
        for identity in ("a/b", "a b", "a?b", "a#b"):
            with self.assertRaisesRegex(ValueError, "path-safe"):
                set_conversation_header(headers, identity)
            with self.assertRaisesRegex(ValueError, "path-safe"):
                conversation_header(identity)
            with self.assertRaisesRegex(ValueError, "path-safe"):
                client.release(identity)
        self.assertEqual(headers, {})

    def test_loopback_http_url_parses(self) -> None:
        ipv4 = PmuxMessages(" http://127.0.0.1:8765/ ")
        self.assertEqual(ipv4.base_url, "http://127.0.0.1:8765")
        self.assertEqual(ipv4.api_key, "pmux")
        ipv6 = PmuxMessages("http://[::1]:8765")
        self.assertEqual(ipv6.base_url, "http://[::1]:8765")
        local = PmuxMessages("http://localhost:8765")
        self.assertEqual(local.base_url, "http://localhost:8765")
        self.assertEqual(PmuxMessages("http://127.0.0.1:8765", api_key="  ").api_key, "pmux")
        self.assertEqual(PmuxMessages("http://127.0.0.1:8765", api_key=" k ").api_key, "k")

    def test_api_key_with_cr_lf_or_nul_is_refused(self) -> None:
        for key in ("a\nb", "a\rb", "a\0b", "a\r\nb"):
            with self.assertRaisesRegex(ValueError, "CR, LF, or NUL"):
                PmuxMessages("http://127.0.0.1:8765", api_key=key)

    def test_https_and_non_loopback_urls_are_refused(self) -> None:
        with self.assertRaisesRegex(ValueError, "http://HOST:PORT"):
            PmuxMessages("https://127.0.0.1:8765")
        with self.assertRaisesRegex(ValueError, "loopback-only"):
            PmuxMessages("http://192.168.1.4:8765")
        with self.assertRaisesRegex(ValueError, "loopback-only"):
            PmuxMessages("http://127.0.0.2:8765")
        with self.assertRaisesRegex(ValueError, "cannot parse"):
            PmuxMessages("http://example.invalid:8765")
        with self.assertRaisesRegex(ValueError, "http://HOST:PORT"):
            PmuxMessages("http://127.0.0.1")
        with self.assertRaisesRegex(ValueError, "must not be empty"):
            PmuxMessages("   ")
        with self.assertRaisesRegex(ValueError, "http://HOST:PORT"):
            PmuxMessages("http://user:pass@127.0.0.1:8765")

    def test_models_capabilities_and_release_speak_http(self) -> None:
        with _RecordingServer() as server:
            host, port = server.server_address
            client = PmuxMessages(f"http://{host}:{port}")
            self.assertEqual(client.models(), {"ok": True, "path": "/v1/models"})
            self.assertEqual(client.capabilities(), {"ok": True, "path": "/v1/capabilities"})
            client.release("abc")
            client.release("sess-2")
            client.release("a!b")
            client.release("100%")
        self.assertEqual(
            [(row["method"], row["path"]) for row in server.seen],
            [
                ("GET", "/v1/models"),
                ("GET", "/v1/capabilities"),
                ("POST", "/v1/conversations/abc/release"),
                ("POST", "/v1/conversations/sess-2/release"),
                ("POST", "/v1/conversations/a!b/release"),
                ("POST", "/v1/conversations/100%/release"),
            ],
        )
        for row in server.seen:
            self.assertEqual(row["x-api-key"], "pmux")
            self.assertEqual(row["authorization"], "Bearer pmux")

    def test_custom_api_key_is_sent_on_both_auth_headers(self) -> None:
        with _RecordingServer() as server:
            host, port = server.server_address
            client = PmuxMessages(f"http://{host}:{port}", api_key="presence")
            client.models()
        self.assertEqual(server.seen[0]["x-api-key"], "presence")
        self.assertEqual(server.seen[0]["authorization"], "Bearer presence")

    def test_non_ok_http_is_surfaced(self) -> None:
        with _RecordingServer(status=500) as server:
            host, port = server.server_address
            client = PmuxMessages(f"http://{host}:{port}")
            with self.assertRaises(PmuxMessagesError) as raised:
                client.models()
            self.assertEqual(raised.exception.status, 500)
            self.assertIn("500", str(raised.exception))
            with self.assertRaises(PmuxMessagesError) as release_error:
                client.release("abc")
            self.assertIn("release 500", str(release_error.exception))

    def test_exchange_times_out_when_the_peer_sends_nothing(self) -> None:
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        port = listener.getsockname()[1]
        held: list[socket.socket] = []

        def hang() -> None:
            try:
                listener.settimeout(5)
                conn, _ = listener.accept()
                held.append(conn)
                time.sleep(5)
            except OSError:
                pass
            finally:
                for conn in held:
                    conn.close()
                listener.close()

        threading.Thread(target=hang, daemon=True).start()
        client = PmuxMessages(f"http://127.0.0.1:{port}", timeout=0.05)
        with self.assertRaises(PmuxMessagesError) as raised:
            client.models()
        self.assertIn("timed out", str(raised.exception).lower())

    def test_exchange_refuses_an_oversized_response(self) -> None:
        class _OversizeHandler(BaseHTTPRequestHandler):
            def log_message(self, format: str, *args: object) -> None:
                del format, args

            def do_GET(self) -> None:
                payload = b"x" * (2 * 1024 * 1024 + 1)
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.send_header("Connection", "close")
                self.end_headers()
                with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                    self.wfile.write(payload)

        server = ThreadingHTTPServer(("127.0.0.1", 0), _OversizeHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            host, port = server.server_address
            client = PmuxMessages(f"http://{host}:{port}")
            with self.assertRaises(PmuxMessagesError) as raised:
                client.models()
            self.assertIn("exceeds", str(raised.exception))
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=4)


if __name__ == "__main__":
    unittest.main()
