#!/usr/bin/env python3
"""对冻结 Headroom 版本执行离线、确定性的压缩与 CCR 保留评测。"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Any, Callable


MARKER_PATTERN = re.compile(r"hash=([a-f0-9]{24})")
IMAGE_DIGEST = (
    "sha256:800a7ead087a791d54b7253c6cd5f98e5964f20fcde42872838f987244e090cc"
)
SOURCE_COMMIT = "6d5516dcb878b6ffd139a1c7b3d480a1c8c1beb9"
SAFETY_RULES = (
    "SAFETY_NEVER_DISCLOSE_PROVIDER_CREDENTIALS",
    "SAFETY_DENY_CROSS_TENANT_RETRIEVAL",
)


@dataclass(frozen=True)
class CorpusCase:
    name: str
    facts: tuple[str, ...]
    content: str


def repeat_lines(factory: Callable[[int], str], count: int) -> str:
    return "\n".join(factory(index) for index in range(count))


def build_corpus() -> list[CorpusCase]:
    code_facts = (
        "AUTHZ_DECISION_DENY_CROSS_TENANT",
        "RETRY_LIMIT_ONE",
        "BODY_LIMIT_4194304",
    )
    code = repeat_lines(
        lambda index: (
            f"fn handler_{index}(tenant: &str) -> Result<u64> {{ "
            f"let shard = {index % 31}; ensure!(tenant.len() > 1); Ok(shard) }}"
        ),
        4_200,
    )
    code += "\n// " + "\n// ".join(code_facts)

    log_facts = (
        "INCIDENT_ID_INC_8421",
        "ROOT_CAUSE_POOL_EXHAUSTION",
        "REGION_AP_SOUTHEAST_1",
    )
    logs = repeat_lines(
        lambda index: (
            f"2026-08-02T03:{index % 60:02d}:00Z level=INFO request=req_{index} "
            f"tenant=t_{index % 41} result=ok latency_ms={index % 173}"
        ),
        5_000,
    )
    logs += "\nlevel=ERROR " + " ".join(log_facts)

    search_facts = (
        "CANONICAL_DOC_DOC_731",
        "OWNER_PLATFORM_SECURITY",
        "ROTATION_DEADLINE_2026_08_15",
    )
    search = repeat_lines(
        lambda index: json.dumps(
            {
                "rank": index,
                "document": f"DOC_{index:04d}",
                "title": f"Gateway operations note {index}",
                "snippet": "Routine operational guidance without a release decision.",
            },
            separators=(",", ":"),
        ),
        4_000,
    )
    search += "\n" + json.dumps(
        {"rank": 7, "facts": search_facts}, separators=(",", ":")
    )

    table_facts = (
        "ACCOUNT_ACCT_9927",
        "BALANCE_CENTS_184250",
        "RISK_TIER_HIGH",
    )
    table = "account,balance_cents,risk_tier,region\n" + repeat_lines(
        lambda index: f"ACCT_{index:04d},{10000 + index},LOW,region_{index % 8}",
        8_000,
    )
    table += "\nACCT_9927,184250,HIGH,region_3\n" + ",".join(table_facts)

    rag_facts = (
        "POLICY_ID_POL_44",
        "RETENTION_DAYS_30",
        "ESCALATION_TEAM_TRUST",
    )
    rag = repeat_lines(
        lambda index: (
            f"[passage_{index}] The gateway handbook section {index} describes routine "
            "deployment checks, health probes, and reversible operational procedures."
        ),
        5_000,
    )
    rag += "\n[authoritative_passage] " + " ".join(rag_facts)

    tool_facts = (
        "CALL_ID_CALL_EVAL_7788",
        "SCHEMA_VERSION_3",
        "DECISION_REJECT",
    )
    tool_json = json.dumps(
        {
            "schema_version": 3,
            "items": [
                {
                    "id": f"item_{index}",
                    "attributes": {
                        "tenant": f"tenant_{index % 53}",
                        "decision": "allow",
                        "score": index % 100,
                    },
                }
                for index in range(2_000)
            ],
            "critical_decision": {
                "call_id": "CALL_EVAL_7788",
                "decision": "REJECT",
                "facts": tool_facts,
            },
        },
        separators=(",", ":"),
    )

    prose_facts = (
        "PROJECT_CODENAME_ORBIT",
        "LAUNCH_WINDOW_2026_Q4",
        "OWNER_TEAM_GATEWAY",
    )
    prose = "\n\n".join(
        (
            f"Chapter {index}. The working group reviewed ordinary service behavior, "
            "documented assumptions, compared alternatives, and recorded follow-up actions. "
            "No decision in this paragraph supersedes the authoritative release note."
        )
        for index in range(1_200)
    )
    prose += "\n\nAuthoritative release note: " + " ".join(prose_facts)

    return [
        CorpusCase("code", code_facts, code),
        CorpusCase("log", log_facts, logs),
        CorpusCase("search", search_facts, search),
        CorpusCase("table", table_facts, table),
        CorpusCase("rag", rag_facts, rag),
        CorpusCase("tool_json", tool_facts, tool_json),
        CorpusCase("prose", prose_facts, prose),
    ]


def request_json(url: str, payload: dict[str, Any] | None = None) -> dict[str, Any]:
    body = None if payload is None else json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=body,
        headers={"content-type": "application/json"} if body is not None else {},
    )
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"{url} 返回 HTTP {error.code}: {detail}") from error


def evaluate_case(base_url: str, case: CorpusCase) -> dict[str, Any]:
    messages = [
        {"role": "system", "content": " ".join(SAFETY_RULES)},
        {
            "role": "user",
            "content": "读取工具结果，严格返回其中三个权威事实；必须遵守系统安全规则。",
        },
        {
            "role": "assistant",
            "tool_calls": [
                {
                    "id": f"call_{case.name}",
                    "type": "function",
                    "function": {"name": f"load_{case.name}", "arguments": "{}"},
                }
            ],
        },
        {
            "role": "tool",
            "tool_call_id": f"call_{case.name}",
            "name": f"load_{case.name}",
            "content": case.content,
        },
    ]
    original = json.dumps(messages, ensure_ascii=False, separators=(",", ":"))
    started = time.perf_counter()
    compressed = request_json(
        f"{base_url}/v1/compress",
        {
            "model": "gpt-4o",
            "config": {"mode": "ccr"},
            "messages": messages,
        },
    )
    latency_ms = round((time.perf_counter() - started) * 1000, 3)
    compressed_text = json.dumps(
        compressed.get("messages", []), ensure_ascii=False, separators=(",", ":")
    )
    hashes = sorted(set(MARKER_PATTERN.findall(compressed_text)))
    retrieved_parts: list[str] = []
    for hash_key in hashes:
        retrieved = request_json(f"{base_url}/v1/retrieve/{hash_key}")
        original_content = retrieved.get("original_content")
        if isinstance(original_content, str):
            retrieved_parts.append(original_content)
    available = compressed_text + "\n" + "\n".join(retrieved_parts)
    baseline_facts_ok = all(fact in original for fact in case.facts)
    inline_facts_ok = all(fact in compressed_text for fact in case.facts)
    ccr_facts_ok = all(fact in available for fact in case.facts)
    safety_ok = all(rule in compressed_text for rule in SAFETY_RULES)
    tool_contract_ok = (
        f"call_{case.name}" in compressed_text and f"load_{case.name}" in compressed_text
    )
    before = int(compressed["tokens_before"])
    after = int(compressed["tokens_after"])
    saved = int(compressed["tokens_saved"])
    if after > before or saved != before - after:
        raise RuntimeError(f"{case.name} 的 token 关系不一致")
    return {
        "name": case.name,
        "tokens_before": before,
        "tokens_after": after,
        "tokens_saved": saved,
        "savings_ratio": round(saved / before, 6) if before else 0.0,
        "compression_latency_ms": latency_ms,
        "ccr_marker_count": len(hashes),
        "baseline_task_success": baseline_facts_ok,
        "compressed_inline_facts": inline_facts_ok,
        "ccr_task_success": ccr_facts_ok and safety_ok and tool_contract_ok,
        "critical_facts_retained": ccr_facts_ok,
        "safety_rules_retained_inline": safety_ok,
        "tool_contract_retained": tool_contract_ok,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:8787")
    args = parser.parse_args()
    cases = [evaluate_case(args.base_url.rstrip("/"), case) for case in build_corpus()]
    savings = [case["savings_ratio"] for case in cases]
    baseline_rate = sum(case["baseline_task_success"] for case in cases) / len(cases)
    ccr_rate = sum(case["ccr_task_success"] for case in cases) / len(cases)
    report = {
        "schema_version": 1,
        "headroom": {
            "source_version": "0.33.0",
            "source_commit": SOURCE_COMMIT,
            "image_digest": IMAGE_DIGEST,
        },
        "evaluator": (
            "deterministic exact-fact, safety-rule and tool-contract oracle with CCR retrieval"
        ),
        "external_llm_used": False,
        "corpus": [case["name"] for case in cases],
        "summary": {
            "cases": len(cases),
            "p50_savings_ratio": round(statistics.median(savings), 6),
            "min_savings_ratio": round(min(savings), 6),
            "baseline_task_success_rate": round(baseline_rate, 6),
            "ccr_task_success_rate": round(ccr_rate, 6),
            "task_success_delta_pp": round((ccr_rate - baseline_rate) * 100, 3),
            "all_critical_facts_retained": all(
                case["critical_facts_retained"] for case in cases
            ),
            "all_safety_rules_retained_inline": all(
                case["safety_rules_retained_inline"] for case in cases
            ),
            "all_tool_contracts_retained": all(
                case["tool_contract_retained"] for case in cases
            ),
            "acceptance_target_met": (
                statistics.median(savings) >= 0.20
                and ccr_rate >= baseline_rate - 0.02
            ),
        },
        "cases": cases,
    }
    print(json.dumps(report, ensure_ascii=False, indent=2, sort_keys=True))
    return 0 if report["summary"]["acceptance_target_met"] else 1


if __name__ == "__main__":
    sys.exit(main())
