# REQ-AI-014 验收报告 — Headroom 上下文压缩与 CCR

> - **验收日期：** 2026-08-02
> - **功能验收：** 通过
> - **生产晋级：** 阻塞；固定 Headroom 镜像存在 1 个 Critical 和 2 个 High
>   的未修复基础镜像漏洞，需上游新 digest 或有时限的正式风险接受
> - **发布形态：** 保持 opt-in，不允许默认开启
> - **需求分析：** [analysis.md](analysis.md)
> - **方案设计：** [design.md](design.md)

## 1. 验收对象

| 项目 | 锁定值 |
|------|--------|
| Headroom 源码版本 | `0.33.0` |
| Headroom commit | `6d5516dcb878b6ffd139a1c7b3d480a1c8c1beb9` |
| Headroom 镜像 | `ghcr.io/headroomlabs-ai/headroom@sha256:800a7ead087a791d54b7253c6cd5f98e5964f20fcde42872838f987244e090cc` |
| 镜像平台 | `linux/arm64` |
| Kong 接入方式 | Route 级原生插件 + 可替换 backend trait + Headroom proxy adapter |
| 透明 CCR 支持 | 非流式 OpenAI Responses、Anthropic Messages |
| 固定旁路 | OpenAI/OpenAI-compatible Chat、streaming、Gemini、受限 `tool_choice`、阈值/大小/path 不符 |
| CCR store | 本地 SQLite 持久卷，默认 TTL 1800s，能力标记 `local` |

## 2. 验收标准结果

| AC | 结果 | 证据摘要 |
|----|------|----------|
| 1. 共享校验、bundled 与插件中心 | 通过 | Admin schema/create/PATCH/PUT/runtime 共用 validator；插件已注册并可在 Manager 配置。 |
| 2. 关闭/旁路 wire 兼容 | 通过 | Chat、streaming、阈值、大小和不支持协议均直连一次 Provider；不改变协议响应。 |
| 3. Responses/Messages 真实 sidecar | 通过 | 通过 Kong `:8000` 到固定镜像、再到 mock Provider 的真实 HTTP 链路；两协议都完成 CCR 续调。 |
| 4. 结构保真 | 通过 | Responses 的 system/developer/content parts/tools/structured output 以及 Anthropic 的 tools/tool use/tool result/metadata 保留。 |
| 5. CCR retrieve 闭环 | 通过 | Provider 首调只见 marker，返回 `headroom_retrieve`；续调看到取回原文；客户端只见 `CCR_CONTRACT_OK`。 |
| 6. 流式/Gemini 安全旁路 | 通过 | `stream=true` 及不支持协议不进入 Headroom，不泄露内部工具。 |
| 7. Header/SSRF/凭据边界 | 通过（实验环境） | 客户端 `x-headroom-*` 全量清理；Provider origin/path 只来自服务端；Provider `Authorization` 保真；sidecar 仅绑定 loopback。 |
| 8. 失败语义/不重放 | 通过 | 预检不可用时 `pass_through` 直连一次、`reject` 在 Provider 前返回 503；派发后失败不再直连。 |
| 9. Token 可观测与脱敏 | 通过 | usage fact/summary、稳定 Kong 响应头及 Prometheus before/after/saved/hop 完整；默认清理 Headroom 头。 |
| 10. 原文治理顺序 | 通过 | Prompt Guard、cache key、TPM/模型窗口在压缩前使用原文；Provider usage 仅用于最终 settle。 |
| 11. Manager 完整流程 | 通过 | Endpoint 发布/编辑/回显/删除、中英文边界提示、usage 压缩 KPI/列表/详情通过 build、lint 和 Playwright。 |
| 12. 冻结语料评测 | 通过 | 7 类冻结语料 P50 节省 99.715%（保守 no-op 也纳入分布）；原文与 CCR 任务成功率均 100%，下降 0pp。 |
| 13. 回归与真实 HTTP | 通过 | 受影响 Rust crate、PG migration/summary、DB-less、Manager 及真实 `:8000` 链路完成验证。 |
| 14. 供应链/部署/回滚 | 证据完整；晋级阻塞 | digest、LICENSE/NOTICE、CycloneDX SBOM、漏洞扫描、volume/TTL、私网与回滚均已归档；扫描有未修复 Critical/High。 |

## 3. 真实 Kong → Headroom → Provider CCR

验收使用真实 Kong DB-less 进程、固定 Headroom 镜像和可编程 Provider，
不用 stub 替代 sidecar：

| 协议 | 输入 | 首次 Provider 调用 | CCR 续调 | 最终结果 |
|------|------|---------------------|----------|----------|
| OpenAI Responses | 约 375 KiB，含 tools 与 structured output | 压缩后结构含 marker，不含原文 sentinel | `function_call_output` 含取回原文 | 只返回 `CCR_CONTRACT_OK` |
| Anthropic Messages | 约 394 KiB，含 tool use/result 与 metadata | 从 96,333 token 压缩到 52，不含原文 sentinel | `tool_result` 含取回原文 | 只返回 `CCR_CONTRACT_OK` |

额外断言：

- Headroom 的两次 Provider 调用都保留 Provider 认证；
- 客户端伪造的 Headroom 目标、bypass 和内部头不到达 Provider；
- Responses 中客户端伪造的同名 retrieve tool 被 Kong 的固定定义覆盖；
- Chat、streaming 和受限 `tool_choice` 都只直连 Provider 一次。

## 4. 质量评测

`scripts/headroom-context-compression-eval.py` 通过 Headroom `/v1/compress`
使用 `ccr` mode 压缩固定的 7 类语料，并从 CCR store 取回 marker 原文做
精确 oracle：

| 语料 | Token 节省率 |
|------|-------------:|
| code | 99.925% |
| log | 99.861% |
| search | 0%（Headroom 保守 no-op） |
| table | 99.903% |
| RAG | 0%（Headroom 保守 no-op） |
| tool JSON | 46.624% |
| prose | 99.715% |

- 7 类冻结语料的 P50：99.715%，两个保守 no-op 样本也纳入分布；排除
  no-op 后的适合压缩集合同样高于 20% 目标；
- 原文基线任务成功率：100%；CCR 任务成功率：100%；差值 0pp；
- 关键事实、安全规则、tool JSON 契约均通过精确断言；
- Headroom 内置 adversarial eval 覆盖 10 种 carrier × 3 个位置 × 7 类
  payload，共 210 个组合；无 payload 使压缩率优于良性基线或绕过抑制。

这些是结构/任务契约评测，未调用外部 LLM，不将合成语料节省率宣称为
生产流量承诺。

## 5. 容量与负载

`scripts/headroom-context-compression-load.py` 使用 4 并发、每档 12 个请求的
4k/32k/128k 矩阵比较 Kong 直连和 Kong + Headroom，全部 0 错误：

| 输入档位 | 链路 | QPS | p50 ms | p95/p99 ms |
|----------|------|----:|-------:|-----------:|
| 4k | Kong 直连 | 10.654 | 223.678 | 683.272 |
| 4k | Kong + Headroom | 2.744 | 226.073 | 4,371.882 |
| 32k | Kong 直连 | 8.170 | 366.705 | 761.419 |
| 32k | Kong + Headroom | 8.703 | 344.223 | 731.875 |
| 128k | Kong 直连 | 5.546 | 656.458 | 905.944 |
| 128k | Kong + Headroom | 2.244 | 1,777.232 | 2,035.352 |

4k Headroom p95/p99 受冷启/孤立尾延迟影响，因此又对 128k 执行了干净、
隔离的 100 请求稳态测试：

| 链路 | QPS | p50 ms | p95 ms | p99 ms | 错误 |
|------|----:|-------:|-------:|-------:|-----:|
| Kong 直连 | 20.313 | 172.648 | 261.472 | 308.191 | 0 |
| Kong + Headroom | 4.041 | 950.982 | 1,324.848 | 1,461.334 | 0 |

资源观测：

- 128k Headroom 稳态负载平均约占 0.84 CPU core，进程 RSS 峰值约
  388 MiB（结束后 `/proc` 约 444 MiB）；
- executor 的 `max_workers=10`，本测试 `queued_max=4`、`in_flight_max=3`、
  `queue_wait_max=28.8ms`、timeout/leak 均为 0；
- 大型离线 eval 曾使 sidecar RSS 增至约 860 MiB，并观测到异常高 CPU。
  重建同 digest sidecar 后恢复为约 138 MiB/idle 0.44%，常规 128k CCR 负载后
  未复现 idle spin。该现象未完成根因分析，生产必须设置 CPU/内存 limit、
  持续监控并做长时 soak。

结论：压缩热路不可按 Kong 直连容量配置；必须按实际上下文档位单独
水平扩容 sidecar，保留 opt-in 和负载保护。

## 6. 故障、持久化与观测

- 预检失败 + `reject`：Kong 返回协议兼容 503，Provider 调用数 0；
- 预检失败 + `pass_through`：Provider 直连调用数 1；
- Headroom 已派发后失败：`HEADROOM_RETRY_MAX_ATTEMPTS=1`，Provider 调用数 1，
  Kong 不进行直连重放；
- SQLite volume 上的 CCR marker 在 sidecar 重启后、TTL 内仍可取回；
- usage fact 保留 status/reason/backend/CCR/before/after/saved/hop，summary 保留状态计数与
  加权压缩率；Manager 对旧 API/全 unknown 不伪造 0 节省。
- Prometheus 使用低基数 `provider/status/reason` 标签，不包含 request ID、租户、
  CCR hash、原文或凭据。

## 7. 供应链验收与生产阻塞

固定镜像 OCI revision 与锁定 commit 一致。仓库保留
`docs/third-party/headroom-0.33.0/LICENSE`、`NOTICE` 和 SBOM 归档信息。镜像内嵌：

- CycloneDX `1.5`，405 个 components；
- 路径：`/usr/local/lib/python3.13/site-packages/headroom_ai-0.33.0.dist-info/sboms/headroom-py.cyclonedx.json`；
- SHA-256：`4e6b9d60b216b145a46b783d122763a05dbde23d89fc0c66633d911d09cde4d6`。

Docker Scout 1.21.0 对精确 digest 扫描 248 个 package，发现 Debian
`perl 5.40.1-6` 的 3 个未修复漏洞：

| 严重性 | CVE | 扫描结果 |
|--------|-----|----------|
| Critical | `CVE-2026-12087` | 未提供 fixed version |
| High | `CVE-2026-48959` | 未提供 fixed version |
| High | `CVE-2026-48962` | 未提供 fixed version |

Headroom 运行时为 Python 进程，本轮没有证明上述 Perl 漏洞存在业务可达路径，
但也没有足够证据出具 VEX `not_affected`。因此不得自动豁免，生产晋级保持
阻塞，直到：

1. Headroom 发布新的、通过同等 contract/eval/load 的无 Critical/High digest；或
2. 安全负责人基于可达性分析签署有到期日的风险接受，并保留私网、
   non-root、read-only rootfs、seccomp、capability drop 和资源限制。

## 8. 已知限制

1. Headroom 0.33.0 的 `HEADROOM_PROXY_TOKEN` 与 Provider Bearer `Authorization`
   冲突，本接入禁用该 token，必须依赖同 Pod loopback、NetworkPolicy 或
   mTLS service mesh。
2. 当请求没有 `Content-Length`且 chunked body 超过 64 KiB 时，当前 Pingora
   请求重放缓冲路径返回显式 `ReadError`；已验证有 `Content-Length` 的约 375 KiB
   body 可完整通过。上线前应由入口强制/normalise `Content-Length`，或单独扩展
   spillable replay buffer。
3. 本地 SQLite 只能声明 `local_retrievable`。多 Pod 必须共享 store 或已验证会话
   粘滞，否则不能声明集群 CCR。
4. Search/RAG 语料在本冻结样本上保守 no-op；这是允许的无损退化，不应
   为追求节省率强制压缩。
5. 观测到离线 eval 后的内存增长/高 CPU 异常，虽未在干净 CCR 负载后复现，
   仍需 soak 和告警门限。

## 9. 工具链基线差异

- `make check`：通过；保留 `responses_format.rs::emitted_added` 和
  `kong-server::DpBgService.config` 两个已有 dead-code warning。
- `make test-dbless`：全工作区通过；只有 2 个显式标注的手工测试 ignored。
- Manager `pnpm lint`：0 error；保留 `HeaderBackButton.vue` 和
  `HeaderEditButton.vue` 的 2 个已有 `vue/no-required-prop-with-default` warning。
- `make lint`：在未受本需求修改的 `kong-core` 基线阻塞；Rust 1.94 Clippy
  将 6 个手写 `Default` impl 报为 `derivable_impls`，且仓库使用
  `-D warnings`。本需求不扩大范围修改这些基线模型。
- `make fmt-check`：仓库大量既有 Rust 文件与当前 Rust 1.94 rustfmt 不一致；
  本需求负责的压缩、usage 和 proxy 文件已用 scoped `rustfmt --check`
  通过，且 `git diff --check` 通过。未运行会重排用户其他改动的全仓格式化。

这两类基线问题不改变 REQ-AI-014 的聚焦测试结果，但在仓库级 CI 要求
`make lint`/`make fmt-check` 必须全绿的环境中仍需单独治理。

## 10. 回归命令

本轮使用的可重复命令：

```bash
cargo test -p kong-ai --locked
cargo test -p kong-config -p kong-plugin-system -p kong-admin -p kong-db \
  -p kong-proxy -p kong-server --locked
KONG_AI_USAGE_PG_TEST_URL=postgres://kong@127.0.0.1:<managed-port>/kong_tests \
  cargo test -p kong-ai usage::postgres::tests::postgres_summary_aggregates_in_database \
  --locked -- --exact
make check
make test-dbless
cd kong-manager && pnpm lint && pnpm build
cd kong-manager && KM_TEST_GUI_URL=http://127.0.0.1:8002 \
  KM_TEST_API_URL=http://127.0.0.1:8001 pnpm exec playwright test \
  tests/playwright/specs/ai-gateway/01-AiGateway.spec.ts \
  tests/playwright/specs/ai-gateway/02-AiUsage.spec.ts
python3 scripts/headroom-context-compression-eval.py --base-url http://127.0.0.1:8787
python3 scripts/headroom-context-compression-load.py --kong-url http://127.0.0.1:8000
docker scout cves --only-severity critical,high \
  ghcr.io/headroomlabs-ai/headroom@sha256:800a7ead087a791d54b7253c6cd5f98e5964f20fcde42872838f987244e090cc
git diff --check
```

## 11. 回滚条件

运行时回滚不需改数据库：禁用或删除 Route 上的
`ai-context-compression` 插件即恢复 Provider 直连。数据库的 forward-only 可空
usage 列可保留，旧行映射为 unknown。Headroom 镜像升级必须使用新 digest，并对
CCR volume 做兼容性检查或独立快照；不使用 `latest` 回滚。
