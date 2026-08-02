# REQ-AI-014 需求分析 — 基于 Headroom 的上下文压缩与 CCR

> Headroom-based Context Compression with CCR — Requirement Analysis
>
> - **优先级 / Effort：** P0 / L
> - **需求分析定稿：** 2026-08-01
> - **实现验证修订：** 2026-08-01（按固定镜像的真实 transport contract 收窄
>   OpenAI Chat，并补充 Responses tool 注入与 sidecar 鉴权边界）
> - **依赖：** 功能上无硬依赖；用户已明确提高优先级并授权先行启动，因而与
>   REQ-AI-003 剩余发布档位验证并行存在于文档状态，但本变更集只实现
>   REQ-AI-014
> - **上游基线：** [`headroomlabs-ai/headroom`](https://github.com/headroomlabs-ai/headroom)
>   commit `6d5516dcb878b6ffd139a1c7b3d480a1c8c1beb9`（源码版本 `0.33.0`，
>   Apache-2.0）
> - **官方镜像：** `ghcr.io/headroomlabs-ai/headroom@sha256:800a7ead087a791d54b7253c6cd5f98e5964f20fcde42872838f987244e090cc`
>   （non-root 多架构 index；OCI revision 与上述 commit 一致）
> - **需求单索引：** [../backlog.md](../backlog.md)

## 背景与价值

Kong-Rust 当前把完整会话、工具结果、检索片段和日志原样发送给模型 Provider。
长上下文直接增加输入 token、成本和首 token 延迟，也可能让模型组在原始估值阶段
因超过 `max_input_tokens` 而无法选中目标模型。

Headroom 提供结构感知、无需额外 LLM 调用的上下文压缩，以及 CCR
（Compress-Cache-Retrieve）：压缩时缓存原文并插入不可猜测的引用；模型需要原文时
调用 `headroom_retrieve`；Headroom 拦截工具调用、取回原文并继续调用 Provider，
最终只把业务响应返回给客户端。

单独调用 Headroom 的 `POST /v1/compress` 只能得到压缩消息和 CCR hash，不能完成
响应侧工具拦截和续调。要交付真实 CCR，首版必须把 Headroom 官方 proxy 作为
Kong-Rust 与已选定 Provider 之间的受控内层跳点。Headroom 不成为面向用户的第二个
网关：Endpoint、Provider/Model 选择、Provider 凭据、Virtual Key、Prompt Guard、
配额、预算、客户端协议转换和最终日志仍由 Kong-Rust 管理。

上游仓库已经包含 Rust `headroom-core`，但直接 SDK 集成仍需由 Kong-Rust 自行实现
`headroom_retrieve` 响应循环。固定镜像的真实验证还发现 proxy transport 存在不对称：
OpenAI Chat direct path 会注入 retrieve tool、却不拦截返回调用；Responses path 会
拦截并续调、却不自动注入扁平工具定义。首版因此选择“原生 Kong 插件 + 官方
Headroom sidecar/proxy adapter”，由 Kong 只为 Responses 补齐确定性的工具定义，
OpenAI Chat 则安全旁路，不引入半成品 SDK，也不伪造 CCR 标记。

## 目标与成功定义

1. 新增 route-scoped `ai-context-compression` 原生插件；插件开启后，符合条件的
   OpenAI Responses 和 Anthropic Messages 非流式请求通过 Headroom sidecar 压缩
   并启用透明 CCR；OpenAI Chat/OpenAI-compatible Chat 在冻结版本上固定旁路。
2. Kong-Rust 先完成原文认证、安全检查、缓存键和配额/预算保守准入，再选择
   Provider；只把服务端解析出的 Provider origin/path 和认证头交给 Headroom，
   客户端不能利用 Headroom 控制头改写目标或读取 CCR store。
3. 关闭插件、低于阈值、流式请求、不支持的 Provider、过大请求或 Headroom
   预检不可用时，按固定原因显式旁路或拒绝；旁路不得改变既有 Provider 请求。
4. Headroom 自动处理 CCR 工具调用与 continuation；Kong-Rust 不向客户端暴露
   内部 `headroom_retrieve` 调用，也不发送无法取回的悬空 marker。
5. 网关能记录 applied/bypassed/degraded/rejected、before/after/saved token 和
   Headroom 延迟，且日志、指标和持久用量不保存上下文正文或凭据。
6. Manager 能在 Endpoint 发布/编辑时配置该策略，并显示实际的配置状态与能力
   边界；文档提供可复现、版本锁定的 sidecar 部署方式。

## 用户故事

- 作为 AI Endpoint 管理员，我可以一键启用 Headroom 上下文压缩与 CCR，并设置
  最小 token 阈值、最大请求大小和 sidecar 不可用时的处理方式。
- 作为应用开发者，我继续使用既有 OpenAI/Anthropic API；如果模型需要被压缩的
  原文，CCR 在网关内部完成，不需要应用实现新工具。
- 作为安全管理员，我可以确认 Prompt Guard 检查的是原文、客户端不能注入
  `x-headroom-base-url` 或读取 retrieve API、Headroom 不记录完整消息。
- 作为运维人员，我可以区分压缩已应用、低于阈值、流式旁路、Provider 不支持、
  协议/tool choice 不支持、sidecar 不健康和压缩失败，并据此决定扩容或回滚。
- 作为成本负责人，我可以比较 Headroom 报告的 before/after token 和 Provider
  最终 usage；准入阶段不会因为未验证的压缩估值而放大额度。

## 术语与执行口径

| 术语 | 本需求定义 |
|------|------------|
| 上下文压缩插件 | route-scoped `ai-context-compression`；只声明策略，不保存正文 |
| Headroom adapter | Kong-Rust 内的后端抽象及官方 proxy 实现；负责健康检查、目标重写和控制头 |
| Headroom sidecar | 版本锁定、自托管的官方 Headroom proxy；压缩并代发一次 Provider 请求，CCR 时可在同一客户端请求内续调 Provider |
| 真实 CCR | 原文成功写入可取回 store、marker 与 `headroom_retrieve` 工具成对注入、工具调用被拦截并完成 continuation；只出现 marker 不算 CCR |
| 原文 token | Kong-Rust 在 Headroom 处理前对客户端请求的保守估值 |
| 压缩后 token | Headroom 响应头 `x-headroom-tokens-after`；只作为观测值，不回写已完成的准入决策 |
| pass-through | 在尚未把请求交给 Headroom/Provider 前安全地走原 Provider；不能在 Provider 是否已被调用不确定时自动重放 |
| fail-closed | 在上游尝试前返回固定 503；已进入 Headroom 后的失败保留确定的 4xx/5xx，不自动二次调用 Provider |

## 现状事实与上游约束（2026-08-01 核实）

### Kong-Rust

- AI 插件顺序是 `ai-key-auth(774) → ai-prompt-guard(773) → ai-cache(772) →
  ai-rate-limit(771) → ai-proxy(770)`；相同 priority 的插件因 `HashMap` 枚举顺序
  不确定，不能用同优先级表达新插件与 `ai-proxy` 的先后关系。
- `ai-proxy` 在 `access` 阶段才解析客户端协议、选择 Provider/Model、生成 Provider
  body 和认证头，并覆写 Pingora 目标。上下文压缩策略必须先保存到
  `RequestCtx.extensions`，实际 Headroom route 只能在 `ai-proxy` 已选定 Provider 后
  应用。
- `ai-rate-limit` 早于 `ai-proxy`，当前按原始 request body 预扣 TPM；模型组也在
  Provider 选定前按原始消息估值。若不增加独立预压缩调用，它们无法使用
  Headroom 的压缩后值。
- 代理默认会转发客户端请求头；若不主动移除，客户端可发送
  `x-headroom-base-url`、`x-headroom-bypass` 等控制头。新实现必须剥离所有
  客户端 `x-headroom-*` 控制头，再加入服务端值。
- Provider response 的 header/body 会进入插件 `header_filter/body_filter/log`，
  因此可在不保存正文的情况下采集 Headroom token headers 和状态。

### Headroom `0.33.0` / commit `6d5516d`

- `/v1/chat/completions`、`/v1/responses` 和 `/v1/messages` proxy 路径都支持压缩，
  但 CCR transport 契约并不对称：OpenAI Chat direct path 会注入 retrieve tool、
  不会拦截其响应；Responses path 会拦截并续调，但不会自行注入扁平工具定义；
  Anthropic Messages 同时具备注入与响应处理。首版只声明经过完整 contract 的档位。
- `POST /v1/compress` 会返回 `messages`、token 统计和 `ccr_hashes`，但不会替调用方
  完成 Provider 响应循环，因此不能单独满足本需求。
- `x-headroom-base-url` 支持逐请求选择 OpenAI/Anthropic-compatible upstream；
  `x-headroom-original-path` 可保留 OpenAI-compatible 的自定义路径。Headroom 会在
  调 Provider 前剥离内部 `x-headroom-*` headers。
- Headroom 会返回 `x-headroom-tokens-before/after/saved`、transforms 和
  `x-headroom-compression-failed`。这些 header 是内部观测契约，Kong-Rust 默认不
  原样暴露给客户端。
- OpenAI streaming handler 明确不执行请求级 CCR intercept；Anthropic 部分路径会
  为 CCR 缓冲或改写流，不能保证现有 SSE 时序、TTFT 和取消语义。因此首版所有
  `stream=true` 请求都旁路 Headroom。
- Native Gemini 不能透明处理 CCR；首版不把 Gemini 列为支持 Provider。
- CCR proxy 默认使用 SQLite store，默认 TTL 1800 秒，可跨进程重启和同卷多 worker
  共享；多 pod 必须使用共享、租户前缀化的 backend adapter 或保证会话粘滞，不能
  把单机 SQLite 宣称为集群级可取回。
- `HEADROOM_PROXY_TOKEN` 在 0.33.0 中优先读取 `Authorization`，会把 OpenAI
  Provider Bearer 凭据误判成 sidecar token 并返回 401；本接入不能启用它。生产必须
  通过同 Pod loopback、专用网络/NetworkPolicy 或保持 Provider header 的 mTLS
  service mesh 隔离 sidecar，并关闭完整消息日志及 Headroom 自带 memory、response
  cache、预算和 rate limit，避免公开访问与重复治理。

## 功能需求

### FR-1 插件与激活条件

1. 新增 bundled Rust 原生插件 `ai-context-compression`，scope 与普通 Kong 插件
   一致；首版产品入口只在包含有效 `ai-proxy` 的 Route/Endpoint 上启用。
2. 配置字段：
   - `min_input_tokens`：`0..=2^31-1`，默认 `2000`；
   - `max_input_bytes`：`1..=16 MiB`，默认 `4 MiB`；
   - `on_unavailable`：`pass_through | reject`，默认 `pass_through`；
   - `streaming`：首版仅允许 `bypass`；
   - `expose_metrics_headers`：布尔值，默认 `false`。
3. Admin create/PATCH/PUT、schema validate 和 runtime 使用同一校验规则；未知字段、
   非法枚举和值越界返回字段级错误。
4. 插件 priority 为 770，`ai-proxy` 调整为 769；既有 guard/cache/rate-limit
   顺序不变。插件只写 policy，实际 route 在 `ai-proxy` 内完成。

### FR-2 全局 Headroom adapter 配置

1. `kong.conf` 新增：
   - `ai_context_compression_headroom_url`：默认空，必须是 `http/https` origin；
   - `ai_context_compression_health_timeout_ms`：默认 200；
   - `ai_context_compression_health_ttl_ms`：默认 1000。
2. sidecar URL 只来自进程配置，Route 插件不能填写任意 URL；Provider 目标只来自
   已解析的 `AiProviderConfig`。这两个约束共同阻断数据面 SSRF 注入。
3. adapter 通过可替换 async trait 暴露 capability、健康预检与目标准备；领域状态
   不包含 reqwest、SQLite、Python 或具体 Headroom store 类型。

### FR-3 请求顺序与安全边界

1. 原文依次经过 `ai-key-auth`、`ai-prompt-guard`、`ai-cache` 和
   `ai-rate-limit`；Prompt Guard 不得只检查压缩后内容，cache key 不得因压缩版本
   变化而漂移。
2. `ai-proxy` 选择实际 Provider/Model并生成 Provider-native body 后才决定是否走
   Headroom。准入和模型窗口首版继续使用原文估值，这是保守上界；不得把 Headroom
   事后 token 统计伪装为准入时已知值。
3. 发送 Headroom 前，移除客户端所有 `x-headroom-*` header。由 Kong-Rust 注入
   `x-headroom-base-url`、必要的 original path，以及低敏的部署标识；Provider 凭据
   沿既有 upstream headers 传入并由 Headroom 转发。
4. 禁止把 Provider 凭据、原始/压缩正文写入
   `log_serialize`、usage fact、trace attribute 或错误体。

### FR-4 支持矩阵与旁路

1. 首版支持：OpenAI Responses 非流式、Anthropic Messages 非流式。Responses 由
   Kong 覆盖客户端同名定义并注入固定的扁平 `headroom_retrieve` tool。
2. 以下固定旁路：OpenAI Chat/OpenAI-compatible Chat、`stream=true`、不允许内部
   retrieve tool 的受限 `tool_choice`、Gemini native、未知 Provider、body 超过
   `max_input_bytes`、原文估值低于 `min_input_tokens`、没有 sidecar 配置、插件未
   启用。每种情况保存一个低基数 reason。
3. 对自定义 Provider path，OpenAI Responses 路径必须以 `/responses` 结尾；
   Anthropic 路径必须以 `/messages` 结尾。无法安全拆分 base/path 时旁路，不猜测和
   改写 URL。
4. 关闭或旁路时，ai-proxy 生成的 target、path、body 和 Provider auth header 与
   本需求引入前一致；除移除危险的客户端 `x-headroom-*` header 外不改变 wire。

### FR-5 CCR 完整性

1. 生产支持档位必须启用 Headroom CCR、tool injection 和 response handler；禁止
   设置 `HEADROOM_NO_CCR=1`，禁止只插 marker 不注入 retrieve tool。
2. Headroom store TTL 必须大于 Endpoint 支持的最长会话间隔；默认部署为 1800 秒，
   UI/文档不得宣称更长保证。
3. 单节点使用持久卷上的 SQLite；多 worker 共享同一文件。多 pod/多节点只有配置
   共享 backend 或会话粘滞并通过跨节点 contract test 后，状态才可标记
   `cluster_retrievable`。
4. retrieve/admin surface 必须只在 loopback 或受 NetworkPolicy/mTLS 隔离的私有
   sidecar 网络可达。冻结版本不能使用与 Provider `Authorization` 冲突的 proxy
   token；Kong-Rust 不向客户端提供通用 retrieve API。
5. tool calling contract 必须验证应用已有 tools 与 `headroom_retrieve` 共存、tool
   ID 不冲突、CCR continuation 完成后内部工具调用不泄露给客户端。

### FR-6 可用性与失败语义

1. adapter 对 `/readyz` 做短超时健康检查并缓存低 TTL 结果；健康缓存仅用于避免
   每请求 I/O，不把不健康结果永久固化。
2. Headroom 在派发前已知不可用时：`pass_through` 直接使用原 Provider target；
   `reject` 返回协议兼容的 503 `context_compression_unavailable`。
3. 请求进入 Headroom 后收到 4xx/5xx、连接中断或结果未知时不得自动改为直连
   Provider，因为 Headroom 可能已经调用 Provider；首版透传/映射确定错误并记录
   `degraded`，避免重复计费和副作用。
4. Headroom 自身报告 `x-headroom-compression-failed=true` 但成功完成 Provider
   请求时，业务响应照常返回，状态记录为 `degraded`，tokens saved 必须为 0 或
   Headroom 报告的非负值。

### FR-7 可观测性

1. 每请求状态至少包含 `applied/bypassed/degraded/rejected`、固定 reason、backend、
   before/after/saved token、compression ratio 和 Headroom 总 hop latency。
2. 从 Headroom header 读取数字时做非负、上限和关系校验：
   `saved <= before`、`after <= before`；坏 header 只使 metrics unavailable，不破坏
   业务响应。
3. 默认移除所有 Headroom response headers；当 `expose_metrics_headers=true` 时只
   输出稳定的 `X-Kong-AI-Context-Compression`、`X-Kong-AI-Tokens-Before/After/Saved`，
   不暴露 transforms、sidecar model、内部错误或 token。
4. Prometheus 标签只使用 provider、status、reason 等枚举；request ID、workspace、
   Route、Consumer、Virtual Key 和 CCR hash 不得作为 label。
5. usage/log 可保存数值与低基数状态，但不保存 messages、tools、tool results、
   retrieval 原文或 CCR hash。

### FR-8 Manager 与文档

1. Endpoint 发布与编辑表单增加上下文压缩开关、阈值、最大字节、不可用策略和
   可观测 header 开关；保存后创建/更新/删除 route-scoped 插件，失败时按既有
   compensating rollback 模式清理。
2. Endpoint 卡片显示 `active/bypassed-by-config/unavailable` 配置态，并明确
   “首版仅 Responses/Messages”“Chat/流式/受限 tool choice 旁路”“准入按原文保守
   计数”“CCR TTL 由 sidecar 决定”。
3. 插件中心的 Rust native schema 可渲染、保存和回显上述字段。
4. 中英文 guide 给出版本/digest 锁定、sidecar 环境变量、CCR store/TTL、网络策略、
   升级回滚和故障排查；不得使用 `latest` 作为生产示例。

## 非功能需求

- **正确性优先：** 任一不确定状态都不能留下不可取回 marker，也不能自动重复
  Provider 调用。
- **兼容性：** 插件关闭/旁路时除安全剥离 `x-headroom-*` 外保持已有协议、body、
  usage 和错误转换。
- **性能：** 健康缓存命中路径不执行额外网络 I/O；启用路径只增加 Headroom hop，
  网关不加载 Python/ONNX 模型、不复制无界消息。
- **隐私：** Headroom 关闭 full-message logging；CCR volume 权限最小化，生产需
  加密卷或等价 at-rest 保护，TTL 到期可验证删除。
- **供应链：** 生产镜像必须固定版本和 digest，保留 Apache-2.0 LICENSE/NOTICE、
  SBOM 和漏洞扫描结果；升级前跑相同 contract/eval。
- **可演进：** adapter capability 明确 `transparent_ccr`、协议与 streaming 支持；
  未来直接 Rust SDK 只有在具备等价 response continuation 后才能替换 proxy adapter。

## 非目标

- 不启用 Headroom memory、MCP、learn、response cache、Provider router、预算或
  rate limit。
- 不让客户端直接配置 Headroom URL、profile、retrieve endpoint 或内部 header。
- 首版不支持 streaming CCR、native Gemini CCR、图片压缩、batch API、WebSocket
  或 Bedrock/Vertex 原生协议。
- 首版不以压缩后估值做模型组准入或 TPM 预扣；若未来需要，必须设计一次且仅一次
  的 pre-compress/CCR store 事务，不能双重压缩。
- 不承诺 Headroom 宣传的固定节省比例或“零精度损失”；以本项目冻结语料评测为准。

## 关联缺陷与风险

1. **双重代理表述风险：** Headroom 确实在内层发起 Provider HTTP；文档必须解释
   这是 CCR 所需的 transport adapter，不能声称完全不代理 Provider。
2. **流式假支持风险：** 压缩 streaming body 不等于能处理响应侧 CCR。首版明确
   bypass，避免把内部 retrieve tool_call 泄露给客户端。
3. **重复调用风险：** Headroom 失败后盲目直连可能重复有副作用的 tool/response
   请求；只允许派发前健康失败时旁路。
4. **多节点 store 风险：** 单个 SQLite volume 不能覆盖多 pod；能力状态必须区分
   `local_retrievable` 与 `cluster_retrievable`。
5. **Header SSRF 风险：** `x-headroom-base-url` 可动态路由；任何客户端同名 header
   必须被覆盖/移除，服务端 URL 需先解析为合法 http/https target。
6. **限额口径差异：** Provider usage 是实际压缩后输入；TPM 准入是原文保守预扣，
   现有 settle 会按实际 usage 退款。UI 必须把 before/after 与 quota admission 分开。
7. **CCR transport 不对称风险：** “能压缩”不等于“能透明续调”。固定镜像中 Chat
   缺响应拦截、Responses 缺工具注入；必须逐协议做真实双调用 contract，不能按同一
   `/v1/*` 路径族推断能力。
8. **sidecar token 冲突风险：** 0.33.0 把 Provider `Authorization` 优先当作 proxy
   token。首版删除该配置，用私有网络/mTLS 建立信任边界，禁止公开 sidecar。

## 决策记录

| ID | 决策 | 理由 |
|----|------|------|
| D1 | 首版采用原生插件 + Headroom 官方 proxy adapter | 当前 Rust SDK 的 OpenAI CCR store/响应循环不完整；proxy 已提供真实 continuation |
| D2 | Headroom 是受控内层 Provider hop | `/v1/compress` 单独无法完成透明 CCR；Kong 仍拥有路由与治理 |
| D3 | 首版只支持非流式 Responses/Anthropic Messages；Chat/streaming 固定旁路 | 只有这两个档位具备可证明的完整 CCR；不能把压缩或工具注入等同于 continuation |
| D4 | 准入和模型窗口按原文保守估值 | sidecar 只在 Provider 已选定后压缩；安全上界优于未经验证的低估 |
| D5 | Headroom URL 是进程级配置；0.33.0 不配置 proxy token | 防止 Route/客户端控制目标；token 与 Provider Authorization 冲突，网络或 mTLS 承担 sidecar 鉴权 |
| D6 | 派发前可旁路，派发后不自动重放 | 避免 Provider 已执行时发生重复计费和副作用 |
| D7 | 所有客户端 `x-headroom-*` header 先清理 | 阻断 SSRF、bypass 和内部信息注入 |
| D8 | sidecar 默认 CCR store 为持久 SQLite，TTL 1800s | 对齐上游当前默认；多节点能力需单独验证，不能过度声明 |
| D9 | 插件 priority 770，ai-proxy 调整为 769 | 保证确定的 policy-before-proxy 顺序，避免同 priority HashMap 非确定性 |
| D10 | P0 先行不混入 REQ-AI-003 代码 | 用户明确提高优先级；保持一个变更集只交付一个 REQ |

## 容量与规模化约束

- **容量包络：** 外层目标 1000 QPS、10k 并发；首个支持档位以每个网关节点一个
  Headroom sidecar 为基线，分别用 4k/32k/128k token 请求测吞吐和尾延迟。
- **热路径 I/O：** 旁路路径除 1s TTL 的健康快照外无额外 I/O；applied 路径增加
  一次到 sidecar 的本地/内网 hop，Provider HTTP 与 CCR continuation 由该 sidecar
  管理。网关不做 Python FFI 或同步磁盘访问。
- **并发与背压：** Headroom `limit-concurrency` 必须有界；网关连接池和健康探测
  timeout 有界。饱和返回 503，不允许在 Kong 或 Headroom 建无界排队。大 body 先按
  字节拒绝/旁路，避免饿死小请求。
- **状态归属：** 插件 policy 属于 Kong 配置；请求状态属于 RequestCtx；原文只属于
  Headroom CCR store，默认 1800s。Kong DB/usage facts 不保存原文或 hash。
- **水平扩展：** per-node sidecar + 本地 SQLite 只标记 local。多节点需共享 CCR
  backend 或会话粘滞、稳定租户前缀、网络隔离和跨节点 retrieve 测试后才可启用
  cluster capability。
- **基数与增长：** Headroom store 以压缩块数增长，至少按 max entries、总字节和
  TTL 三维限制；Kong metrics 不以 CCR hash/tenant/request 作为标签。
- **故障验证：** 覆盖 sidecar 未启动、健康超时、连接失败、503、坏 metrics headers、
  compression-failed、CCR store 重启、TTL 过期、sidecar 网络暴露检查、Provider 在
  首次调用与 continuation 调用失败、客户端取消。
- **负载口径：** 分别报告 Kong-only 与 Kong+Headroom 的 QPS、CPU/内存、p50/p95/p99
  latency、TTFT、sidecar queue/saturation、token savings、CCR retrieve 次数、旁路率
  和错误率；不以单个最佳样例代替分布。

## 验收标准

1. Admin schema/create/update/runtime 对插件字段使用同一校验；插件在 bundled 列表和
   Manager 插件中心可见。
2. 插件关闭或固定旁路时，OpenAI/Anthropic/OpenAI-compatible 的 Provider target、
   path、body 和响应转换与基线一致。
3. 非流式 OpenAI Responses 与 Anthropic Messages 经真实 Headroom sidecar 到 mock
   Provider；Provider 收到压缩后的合法结构。OpenAI Chat/OpenAI-compatible Chat
   断言以 `unsupported_protocol` 旁路且内部工具调用不泄露。
4. contract fixture 覆盖 system/developer、最新 user、content parts、已有 tools、
   tool call/result ID 与 structured output，压缩后不出现非法 role 或 JSON/schema
   损坏。
5. 至少一个请求触发 `headroom_retrieve`；Headroom 从 store 取回原文、续调 mock
   Provider，客户端只收到最终业务响应且 Provider 调用次数/内容符合预期。
6. 所有 `stream=true` 与 native Gemini 请求都以固定 reason 旁路，既有 SSE/TTFT/
   usage 测试通过，客户端看不到 CCR 工具。
7. 客户端注入任意 `x-headroom-*` header 不能改变 sidecar、Provider origin/path 或
   bypass；mock Provider 也收不到这些内部 header。sidecar 不公开暴露，Provider
   `Authorization` 能原样到达 mock Provider。
8. sidecar 预检不可用时分别验证 pass-through 与 503 reject；请求已进入 Headroom 后
   的不确定失败不自动直连 Provider。
9. Headroom token headers 合法时形成 before/after/saved 和 ratio；坏 header 只将
   metrics 标记 unavailable；默认下游不含原始 `x-headroom-*`。
10. Prompt Guard 对仅在原文出现的 deny pattern 仍阻断；cache key 与插件关闭时相同；
    TPM 准入使用原文估值、最终 settle 使用 Provider usage 的契约测试通过。
11. Manager 发布/编辑/回显/删除上下文压缩策略通过 lint、build、E2E 和浏览器可见
    流程验证，文案明确 Responses/Messages、Chat/流式旁路与 CCR TTL 边界。
12. 用冻结代表性语料报告 token savings 与任务成功率；适合压缩的集合 P50 savings
    目标不低于 20%，关键事实/安全规则/任务成功率相对基线下降不超过 2 个百分点。
    目标未达成时功能保持 opt-in，不伪造已验收。
13. `cargo check/test` 覆盖受影响 crate，真实 8000 HTTP E2E 可复现；Manager 检查与
    `git diff --check` 通过。
14. 生产部署示例固定 Headroom version + image digest，包含 LICENSE/NOTICE、SBOM、
    漏洞扫描、CCR volume/TTL、网络策略和一键回滚说明；禁止使用 `latest`。
