# REQ-AI-014 实现记录：Headroom 上下文压缩与 CCR

- 日期：2026-08-01 — 2026-08-02
- 状态：功能验收通过；固定镜像的 Critical/High 漏洞使生产晋级保持阻塞
- 上游：`headroomlabs-ai/headroom@6d5516dcb878b6ffd139a1c7b3d480a1c8c1beb9`
- 镜像：`ghcr.io/headroomlabs-ai/headroom@sha256:800a7ead087a791d54b7253c6cd5f98e5964f20fcde42872838f987244e090cc`

## 本轮实现

1. 新增 Route 级原生插件 `ai-context-compression`，优先级 770；`ai-proxy` 调整为
   769，在 Provider 已选定后应用压缩策略。
2. 在 `kong-ai` 增加可替换 `ContextCompressionBackend` trait 与官方 Headroom
   proxy adapter，包含 `/readyz` 短 TTL 健康检查、Provider target 校验和原子路由
   覆写。
3. 冻结版本首版透明 CCR 支持非流式 OpenAI Responses 与 Anthropic Messages：
   - Responses：Kong 覆盖客户端同名定义并注入固定的扁平
     `headroom_retrieve` tool；Headroom 负责取回和 continuation；
   - Anthropic Messages：使用 Headroom 原生 tool injection 与 response handler；
   - OpenAI/OpenAI-compatible Chat：以 `unsupported_protocol` 固定旁路，防止内部
     retrieve tool call 泄露给客户端；
   - streaming、Gemini、受限 `tool_choice`、阈值/大小/path 不符合时固定旁路。
4. 清理所有客户端 `x-headroom-*` 控制头，目标 URL/path 只由已解析的 Provider
   target 生成；默认移除 Headroom 响应头，可选择公开稳定的 Kong before/after/saved
   头，并写入低敏结构化日志字段。
5. 增加 `kong.conf`、共享 validator、Admin schema/bundled/server 注册与脱敏
   `/status` capability；状态只公开 backend、支持协议、CCR、streaming 和 local store
   scope，不公开 URL。
6. Kong Manager Endpoint 发布/编辑/删除流程已接入插件，包含阈值、最大字节、不可用
   策略、稳定指标头与配置态 badge；usage 页增加节省 Token、加权压缩率、
   旁路率、请求状态和 CCR 详情。
7. 中英文 AI Gateway guide、架构与需求文档已加入固定 digest、SQLite CCR volume、
   私有网络、升级和回滚说明。

## 固定镜像真实 CCR 证据

使用官方 digest、SQLite volume、`--mode token --no-cache --no-rate-limit` 和可编程
mock Provider 执行非流式双调用 contract：

| 协议 | 首次 Provider 请求 | continuation 请求 | 客户端结果 |
|------|---------------------|-------------------|------------|
| OpenAI Responses | 约 375 KiB 原始请求被压缩为 CCR marker，不含原文 sentinel | `function_call_output` 含取回原文 sentinel | 只收到 `CCR_CONTRACT_OK` |
| Anthropic Messages | 约 394 KiB，96,333 token 压缩到 52，不含原文 sentinel | `tool_result` 含取回原文 sentinel | 只收到 `CCR_CONTRACT_OK` |

Anthropic 本次样例由 96,333 token 压缩到 52 token，Headroom 报告节省 96,281。
该数字只证明本合成样例的 transport contract，不代表生产质量或节省承诺。

真实验证同时发现并收敛了两个上游边界：

- Headroom 0.33.0 的 direct OpenAI Chat transport 会注入 retrieve tool，但不会拦截
  Provider 返回的调用，因此不能声明透明 CCR；
- `HEADROOM_PROXY_TOKEN` 优先读取 `Authorization`，会与 OpenAI Provider Bearer
  凭据冲突并返回 401。本冻结接入不配置该 token，必须用同 Pod loopback、专用
  NetworkPolicy/防火墙或保持 Provider header 的 mTLS service mesh 保护 sidecar。

## 完整验收

2026-08-02 已补齐 usage migration/detail/summary、Prometheus、真实 Kong `:8000`
全链路、Manager CRUD/usage UI、SQLite 跨重启、故障矩阵、冻结语料评测、负载测试、
PG/DB-less 回归和供应链归档。命令、分项结果、性能数据、已知限制和回滚条件见
[REQ-AI-014 验收报告](../pm/REQ-AI-014/acceptance.md)。

功能验收通过，但 Docker Scout 对固定 digest 发现 Debian `perl 5.40.1-6`
的 1 个 Critical 和 2 个 High 未修复漏洞。在新镜像通过同等回归或安全负责人
正式接受风险前，生产晋级保持阻塞，功能保持 opt-in。

运行时回滚方式是禁用或删除 Route 上的 `ai-context-compression` 插件。使用
`on_unavailable=pass_through` 时，派发前的 sidecar 不可用也会保持原 Provider
直连；请求一旦进入 Headroom，不做可能重复计费或产生副作用的自动重放。
