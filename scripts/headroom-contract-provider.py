#!/usr/bin/env python3
"""Headroom/Kong 端到端验收用的最小 Provider mock。"""

from __future__ import annotations

import argparse
import json
import re
import socket
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any


MARKER_PATTERNS = (
    re.compile(r"hash=([a-f0-9]{24})"),
    re.compile(r"<<ccr:([a-f0-9]{12,24})\b"),
    re.compile(r"Retrieve original: hash=([a-f0-9]{12,24})"),
)


class ContractState:
    def __init__(self, sentinel: str) -> None:
        self.sentinel = sentinel
        self.lock = threading.Lock()
        self.calls: list[dict[str, Any]] = []

    def append(self, observation: dict[str, Any]) -> None:
        with self.lock:
            self.calls.append(observation)

    def snapshot(self) -> dict[str, Any]:
        with self.lock:
            calls = list(self.calls)
        return {
            "request_count": len(calls),
            "responses_calls": sum(call["protocol"] == "responses" for call in calls),
            "anthropic_calls": sum(call["protocol"] == "anthropic" for call in calls),
            "calls": calls,
        }

    def reset(self) -> None:
        with self.lock:
            self.calls.clear()


def walk_strings(value: Any):
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for item in value:
            yield from walk_strings(item)
    elif isinstance(value, dict):
        for item in value.values():
            yield from walk_strings(item)


def extract_marker(value: Any) -> str | None:
    for text in walk_strings(value):
        for pattern in MARKER_PATTERNS:
            match = pattern.search(text)
            if match:
                return match.group(1)
    return None


def has_ccr_result(value: Any, protocol: str) -> bool:
    if isinstance(value, list):
        return any(has_ccr_result(item, protocol) for item in value)
    if not isinstance(value, dict):
        return False
    if protocol == "responses":
        if (
            value.get("type") == "function_call_output"
            and value.get("call_id") == "call_contract_retrieve"
        ):
            return True
    elif (
        value.get("type") == "tool_result"
        and value.get("tool_use_id") == "toolu_contract_retrieve"
    ):
        return True
    return any(has_ccr_result(item, protocol) for item in value.values())


def count_named_tools(value: Any, name: str) -> int:
    tools = value.get("tools") if isinstance(value, dict) else None
    if not isinstance(tools, list):
        return 0
    count = 0
    for tool in tools:
        if not isinstance(tool, dict):
            continue
        nested = tool.get("function")
        tool_name = tool.get("name")
        if tool_name is None and isinstance(nested, dict):
            tool_name = nested.get("name")
        if tool_name == name:
            count += 1
    return count


class ContractHandler(BaseHTTPRequestHandler):
    server: "ContractServer"

    def log_message(self, fmt: str, *args: Any) -> None:
        # 验收日志只输出状态，不打印可能包含原文的请求体。
        print(f"provider-mock: {fmt % args}", flush=True)

    def do_GET(self) -> None:
        if self.path == "/stats":
            self._json(200, self.server.state.snapshot())
        elif self.path in ("/health", "/readyz"):
            self._json(200, {"status": "ok"})
        else:
            self._json(404, {"error": "not_found"})

    def do_POST(self) -> None:
        if self.path == "/reset":
            self.server.state.reset()
            self._json(200, {"status": "reset"})
            return
        if self.path.endswith("/responses"):
            self._provider_request("responses")
            return
        if self.path.endswith("/messages"):
            self._provider_request("anthropic")
            return
        self._json(404, {"error": "unsupported_path"})

    def _provider_request(self, protocol: str) -> None:
        length = int(self.headers.get("content-length", "0"))
        self.connection.settimeout(2)
        chunks: list[bytes] = []
        remaining = length
        try:
            while remaining > 0:
                chunk = self.rfile.read1(min(65_536, remaining))
                if not chunk:
                    break
                chunks.append(chunk)
                remaining -= len(chunk)
        except (socket.timeout, TimeoutError):
            pass
        raw = b"".join(chunks)
        if len(raw) != length:
            self.server.state.append(
                {
                    "protocol": protocol,
                    "incomplete_body": True,
                    "expected_body_bytes": length,
                    "received_body_bytes": len(raw),
                }
            )
            self._json(408, {"error": "incomplete_request_body"})
            return
        try:
            body = json.loads(raw)
        except json.JSONDecodeError:
            self._json(400, {"error": "invalid_json"})
            return

        serialized = json.dumps(body, ensure_ascii=False, separators=(",", ":"))
        marker = extract_marker(body)
        continuation = has_ccr_result(body, protocol)
        metadata = body.get("metadata")
        load_test = isinstance(metadata, dict) and metadata.get("load_test") is True
        headroom_headers = sorted(
            name.lower() for name in self.headers if name.lower().startswith("x-headroom-")
        )
        observation = {
            "protocol": protocol,
            "body_bytes": len(raw),
            "provider_auth_ok": (
                self.headers.get("authorization") == "Bearer provider-secret"
                if protocol == "responses"
                else self.headers.get("x-api-key") == "provider-secret"
            ),
            "headroom_headers": headroom_headers,
            "sentinel_seen": self.server.state.sentinel in serialized,
            "marker_seen": marker is not None,
            "marker_hash": marker,
            "continuation": continuation,
            "retrieve_tool_count": count_named_tools(body, "headroom_retrieve"),
            "existing_tool_count": count_named_tools(body, "existing_tool"),
            "system_role_seen": '"role":"system"' in serialized,
            "developer_role_seen": '"role":"developer"' in serialized,
            "latest_user_seen": "LATEST_USER_CONTRACT" in serialized,
            "content_parts_seen": (
                '"type":"input_text"' in serialized or '"type":"text"' in serialized
            ),
            "existing_call_id_seen": "call_existing_contract" in serialized,
            "structured_output_seen": "contract_output_schema" in serialized,
            "load_test": load_test,
        }
        self.server.state.append(observation)

        if body.get("model") == "fail-after-dispatch":
            self._json(502, {"error": {"message": "forced provider failure"}})
            return

        # 负载基线使用受限 tool_choice 触发 Kong 旁路；mock 在原文到达时直接完成，
        # 以便和相同请求体的 Headroom+CCR 两跳路径比较。
        if isinstance(metadata, dict) and metadata.get("load_baseline") is True:
            if protocol == "responses":
                self._responses_final(body)
            else:
                self._anthropic_final(body)
            return
        if (
            load_test and marker is None
        ):
            # 短上下文可能由 Headroom 判定为无需变换，这仍是成功的 applied hop。
            if protocol == "responses":
                self._responses_final(body)
            else:
                self._anthropic_final(body)
            return

        if continuation:
            if self.server.state.sentinel not in serialized:
                self._json(500, {"error": "ccr_continuation_missing_original"})
                return
            if protocol == "responses":
                self._responses_final(body)
            else:
                self._anthropic_final(body)
            return

        if marker is None or (self.server.state.sentinel in serialized and not load_test):
            self._json(422, {"error": "request_was_not_safely_compressed"})
            return
        if protocol == "responses":
            self._responses_retrieve(body, marker)
        else:
            self._anthropic_retrieve(body, marker)

    def _responses_retrieve(self, body: dict[str, Any], marker: str) -> None:
        self._json(
            200,
            {
                "id": "resp_contract_retrieve",
                "object": "response",
                "created_at": int(time.time()),
                "status": "completed",
                "model": body.get("model", "contract-model"),
                "output": [
                    {
                        "type": "function_call",
                        "id": "fc_contract_retrieve",
                        "call_id": "call_contract_retrieve",
                        "name": "headroom_retrieve",
                        "arguments": json.dumps({"hash": marker}),
                        "status": "completed",
                    }
                ],
                "usage": {"input_tokens": 80, "output_tokens": 8, "total_tokens": 88},
            },
        )

    def _responses_final(self, body: dict[str, Any]) -> None:
        self._json(
            200,
            {
                "id": "resp_contract_final",
                "object": "response",
                "created_at": int(time.time()),
                "status": "completed",
                "model": body.get("model", "contract-model"),
                "output": [
                    {
                        "type": "message",
                        "id": "msg_contract_final",
                        "status": "completed",
                        "role": "assistant",
                        "content": [
                            {
                                "type": "output_text",
                                "text": "CCR_CONTRACT_OK",
                                "annotations": [],
                            }
                        ],
                    }
                ],
                "usage": {"input_tokens": 120, "output_tokens": 5, "total_tokens": 125},
            },
        )

    def _anthropic_retrieve(self, body: dict[str, Any], marker: str) -> None:
        self._json(
            200,
            {
                "id": "msg_contract_retrieve",
                "type": "message",
                "role": "assistant",
                "model": body.get("model", "contract-model"),
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_contract_retrieve",
                        "name": "headroom_retrieve",
                        "input": {"hash": marker},
                    }
                ],
                "stop_reason": "tool_use",
                "stop_sequence": None,
                "usage": {"input_tokens": 80, "output_tokens": 8},
            },
        )

    def _anthropic_final(self, body: dict[str, Any]) -> None:
        self._json(
            200,
            {
                "id": "msg_contract_final",
                "type": "message",
                "role": "assistant",
                "model": body.get("model", "contract-model"),
                "content": [{"type": "text", "text": "CCR_CONTRACT_OK"}],
                "stop_reason": "end_turn",
                "stop_sequence": None,
                "usage": {"input_tokens": 120, "output_tokens": 5},
            },
        )

    def _json(self, status: int, value: Any) -> None:
        payload = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


class ContractServer(ThreadingHTTPServer):
    def __init__(self, address: tuple[str, int], state: ContractState) -> None:
        super().__init__(address, ContractHandler)
        self.state = state


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=19090)
    parser.add_argument("--sentinel", default="CCR_ORIGINAL_SENTINEL_7f33d829")
    args = parser.parse_args()
    server = ContractServer((args.host, args.port), ContractState(args.sentinel))
    print(f"provider-mock listening on {args.host}:{args.port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
