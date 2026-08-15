#!/usr/bin/env python3
"""Deterministic OpenAI-compatible model used by the A2A progress live test."""

from __future__ import annotations

import json
import re
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

TOKEN = re.compile(r"a2a-ci-inbound-progress-[0-9a-f]{12}")
PROGRESS_PROMPT = "Write one concise progress update"
PROGRESS_TEXT = "I'm working through the requested calculation."


def request_text(request: dict[str, Any]) -> str:
    return json.dumps(request, separators=(",", ":"))


def final_text(request: dict[str, Any]) -> str:
    match = TOKEN.search(request_text(request))
    token = match.group(0) if match else "a2a-ci-inbound-progress-missing"
    return f"2 + 2 = 4; 3 + 3 = 6; 4 + 6 = 10. {token}"


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args: Any) -> None:
        return

    def json_response(self, text: str, model: str) -> None:
        body = json.dumps(
            {
                "id": "chatcmpl-a2a-progress",
                "object": "chat.completion",
                "created": 0,
                "model": model,
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": text},
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2,
                },
            }
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def stream_main_response(self, request: dict[str, Any], model: str) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()

        def chunk(delta: dict[str, Any], finish_reason: str | None = None) -> None:
            payload = {
                "id": "chatcmpl-a2a-progress",
                "object": "chat.completion.chunk",
                "model": model,
                "choices": [
                    {"index": 0, "delta": delta, "finish_reason": finish_reason}
                ],
            }
            self.wfile.write(f"data: {json.dumps(payload)}\n\n".encode())
            self.wfile.flush()

        chunk({"role": "assistant"})
        remaining = 125
        while remaining > 0:
            pause = min(5, remaining)
            time.sleep(pause)
            remaining -= pause
            chunk({})
        chunk({"content": final_text(request)})
        chunk({}, "stop")
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def do_POST(self) -> None:  # noqa: N802 - stdlib handler API
        length = int(self.headers.get("Content-Length", "0"))
        request = json.loads(self.rfile.read(length) or b"{}")
        model = str(request.get("model") or "a2a-progress-model")
        if PROGRESS_PROMPT in request_text(request):
            self.json_response(PROGRESS_TEXT, model)
        elif request.get("stream"):
            self.stream_main_response(request, model)
        else:
            time.sleep(125)
            self.json_response(final_text(request), model)


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8088
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
