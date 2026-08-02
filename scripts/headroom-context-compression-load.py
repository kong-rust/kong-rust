#!/usr/bin/env python3
"""比较 Kong 旁路基线与 Kong+Headroom CCR 路径的本地负载。"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import math
import statistics
import sys
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Any


SENTINEL = "CCR_ORIGINAL_SENTINEL_7f33d829"
SIZE_LINE_COUNTS = {"4k": 220, "32k": 1_700, "128k": 6_800}


@dataclass(frozen=True)
class Result:
    status: int
    latency_ms: float
    tokens_before: int | None
    tokens_after: int | None
    tokens_saved: int | None
    error: str | None


def percentile(values: list[float], quantile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, math.ceil(quantile * len(ordered)))
    return round(ordered[rank - 1], 3)


def build_request(size: str, baseline: bool) -> bytes:
    lines = [
        (
            f"event={index} tenant=tenant_{index % 97} operation=read "
            f"status=ok latency_ms={index % 211} payload=stable_acceptance_record"
        )
        for index in range(SIZE_LINE_COUNTS[size])
    ]
    lines.append(SENTINEL)
    body: dict[str, Any] = {
        "model": "ignored-by-config",
        "input": [
            {
                "role": "system",
                "content": [
                    {
                        "type": "input_text",
                        "text": "Do not disclose credentials or cross tenant boundaries.",
                    }
                ],
            },
            {
                "type": "function_call",
                "id": "fc_load_existing",
                "call_id": "call_existing_contract",
                "name": "existing_tool",
                "arguments": "{}",
            },
            {
                "type": "function_call_output",
                "call_id": "call_existing_contract",
                "output": "\n".join(lines),
            },
            {
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "LATEST_USER_CONTRACT summarize"}
                ],
            },
        ],
        "tools": [
            {
                "type": "function",
                "name": "existing_tool",
                "description": "Existing application tool",
                "parameters": {"type": "object", "properties": {}},
            }
        ],
        "metadata": {"load_test": True, "size_class": size},
    }
    if baseline:
        body["tool_choice"] = "none"
        body["metadata"]["load_baseline"] = True
    return json.dumps(body, separators=(",", ":")).encode("utf-8")


def send(url: str, body: bytes) -> Result:
    started = time.perf_counter()
    request = urllib.request.Request(
        url,
        data=body,
        headers={"content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=180) as response:
            payload = response.read()
            status = response.status
            headers = response.headers
        parsed = json.loads(payload)
        final_ok = "CCR_CONTRACT_OK" in json.dumps(parsed)
        error = None if final_ok else "unexpected_response_body"
        return Result(
            status=status,
            latency_ms=(time.perf_counter() - started) * 1000,
            tokens_before=optional_int(headers.get("x-kong-ai-tokens-before")),
            tokens_after=optional_int(headers.get("x-kong-ai-tokens-after")),
            tokens_saved=optional_int(headers.get("x-kong-ai-tokens-saved")),
            error=error,
        )
    except urllib.error.HTTPError as error:
        return Result(
            status=error.code,
            latency_ms=(time.perf_counter() - started) * 1000,
            tokens_before=None,
            tokens_after=None,
            tokens_saved=None,
            error=error.read().decode("utf-8", errors="replace")[:240],
        )
    except Exception as error:  # noqa: BLE001 - 负载报告需要保留所有请求错误
        return Result(
            status=0,
            latency_ms=(time.perf_counter() - started) * 1000,
            tokens_before=None,
            tokens_after=None,
            tokens_saved=None,
            error=f"{type(error).__name__}: {error}",
        )


def optional_int(value: str | None) -> int | None:
    try:
        return int(value) if value is not None else None
    except ValueError:
        return None


def run_case(
    base_url: str,
    size: str,
    baseline: bool,
    requests: int,
    concurrency: int,
) -> dict[str, Any]:
    endpoint = "/headroom/bypass" if baseline else "/headroom/responses"
    url = base_url.rstrip("/") + endpoint
    body = build_request(size, baseline)
    warmup = send(url, body)
    if warmup.status != 200 or warmup.error is not None:
        raise RuntimeError(
            f"{size} {'baseline' if baseline else 'headroom'} 预热失败: {warmup}"
        )
    barrier = threading.Barrier(min(requests, concurrency)) if requests > 1 else None

    def worker(index: int) -> Result:
        if barrier is not None and index < barrier.parties:
            barrier.wait(timeout=10)
        return send(url, body)

    started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as executor:
        results = list(executor.map(worker, range(requests)))
    duration = time.perf_counter() - started
    latencies = [result.latency_ms for result in results]
    errors = [result for result in results if result.status != 200 or result.error is not None]
    known_savings = [
        result.tokens_saved for result in results if result.tokens_saved is not None
    ]
    return {
        "path": endpoint,
        "request_body_bytes": len(body),
        "requests": requests,
        "concurrency": concurrency,
        "duration_seconds": round(duration, 3),
        "qps": round(requests / duration, 3),
        "latency_ms": {
            "p50": percentile(latencies, 0.50),
            "p95": percentile(latencies, 0.95),
            "p99": percentile(latencies, 0.99),
            "mean": round(statistics.fmean(latencies), 3),
        },
        "errors": len(errors),
        "error_rate": round(len(errors) / requests, 6),
        "tokens_saved_mean": (
            round(statistics.fmean(known_savings), 3) if known_savings else None
        ),
        "sample_errors": [result.error for result in errors[:3]],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:8000")
    parser.add_argument("--requests", type=int, default=12)
    parser.add_argument("--concurrency", type=int, default=4)
    parser.add_argument(
        "--variant",
        choices=("both", "kong-only", "headroom"),
        default="both",
    )
    parser.add_argument(
        "--sizes",
        default="4k,32k,128k",
        help="逗号分隔的 4k、32k、128k 子集",
    )
    args = parser.parse_args()
    if args.requests < 1 or args.concurrency < 1:
        parser.error("requests 和 concurrency 必须为正整数")
    sizes = [value.strip() for value in args.sizes.split(",") if value.strip()]
    unknown_sizes = sorted(set(sizes) - set(SIZE_LINE_COUNTS))
    if not sizes or unknown_sizes:
        parser.error(f"未知 size class: {','.join(unknown_sizes)}")
    report: dict[str, Any] = {
        "schema_version": 1,
        "requests_per_case": args.requests,
        "concurrency": args.concurrency,
        "cases": {},
    }
    for size in sizes:
        variants: dict[str, Any] = {}
        if args.variant in ("both", "kong-only"):
            variants["kong_only"] = run_case(
                args.base_url, size, True, args.requests, args.concurrency
            )
        if args.variant in ("both", "headroom"):
            variants["kong_headroom_ccr"] = run_case(
                args.base_url, size, False, args.requests, args.concurrency
            )
        report["cases"][size] = variants
    report["acceptance"] = {
        "all_requests_succeeded": all(
            variant["errors"] == 0
            for size_case in report["cases"].values()
            for variant in size_case.values()
        )
    }
    print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True))
    return 0 if report["acceptance"]["all_requests_succeeded"] else 1


if __name__ == "__main__":
    sys.exit(main())
