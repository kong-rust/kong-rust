# Kong-Rust Agent 指南

本文件是编码 Agent 的仓库级权威规范（source of truth），适用于整个仓库，
除非更近目录层级的 `AGENTS.md` 提供了更具体的指令。

## 工作约定

- 保留脏工作区中与任务无关的用户改动。
- 优先小而可审查的 diff。除非任务明确要求且产生的变更经过审查，否则不要
  运行仓库级格式化。
- 仓库搜索使用 `rg` 和 `rg --files`。
- 在新增抽象或依赖之前，优先复用现有模块、helper、schema 和测试模式。
- 严禁提交凭据、生成的密钥、本地数据库、日志或 agent 运行时状态。
- AI Gateway 代码和文档中对 Anthropic / Claude 模型的引用属于产品兼容性
  内容，不是遗留的 Claude Code 配置。

## 任务执行策略

- 对审查、解释、诊断、状态或规划类请求：检查相关材料并给出有证据支撑的
  结论。除非请求同时要求，否则不要实施改动。
- 对变更、构建或修复类请求：完成范围内的本地编辑并运行相关的非破坏性
  检查，无需暂停等待确认。
- 以下操作先询问：外部写入、破坏性操作、购买、凭据或权限变更、范围的
  实质性扩大。
- 能从仓库安全获取的细节自行解决，不要问用户。只在缺失的选择会实质性
  改变结果时才提问。
- 交付汇报以结论开头，包含支撑证据、重要注意事项、已执行的验证，以及
  尚存的下一步动作。

## 需求交付流程

- 功能开发以 `docs/pm/backlog.md` 中的需求单（REQ）为单位管理。一次交付
  一个需求；除非用户明确要求合并，否则不要在一个变更集中混合多个需求单。
- 每个需求一个文件夹 `docs/pm/REQ-XXX/`，依次通过三道门禁：
  1. **需求分析** — 目标、范围（后端/前端/文档）、验收标准定稿写入
     `docs/pm/REQ-XXX/analysis.md`。
  2. **方案设计** — 编码前确定技术方案，写入 `docs/pm/REQ-XXX/design.md`。
  3. **编码实现** — 代码、测试和文档。
  前一道门禁未完成不得开始编码。`docs/pm/backlog.md` 中的需求单只保留
  摘要、状态与两份文档的链接。
- Kong Manager（前端）支持与后端能力在同一需求单内同步交付。仅当需求单
  显式将前端范围标注为豁免并说明原因时，才允许纯后端交付。
- 门禁完成即更新需求单状态；跟踪的实现状态变化时同步 `docs/tasks.md`。

## 项目地图

Kong-Rust 是基于 Pingora 的 Kong Gateway Rust 2021 重写。工作区当前在
Rust 1.94 下验证。

```text
crates/
├── kong-core            核心模型与 trait
├── kong-config          kong.conf 解析
├── kong-db              PostgreSQL DAO、缓存、DB-less 模式、迁移
├── kong-router          traditional 与 expressions 路由器
├── kong-proxy           Pingora HTTP/stream 代理
├── kong-plugin-system   插件注册与阶段执行
├── kong-lua-bridge      Lua 兼容层与 PDK
├── kong-admin           Axum Admin API
├── kong-cluster         CP/DP 集群通信
├── kong-ai              AI Gateway provider、codec、路由、计量
└── kong-server          主二进制与运行时装配
```

其他重要路径：

- `kong-manager/` — Vue 3 + TypeScript 管理界面。
- `scripts/` — 测试运行器与依赖服务编排。
- `spec/` — Kong 兼容的 Lua/spec 测试。
- `docs/` — 需求、设计、任务跟踪、指南与实现日志。

crate 依赖必须保持从低层 crate 指向 `kong-server` 的单向关系，避免循环
依赖。

## 设计约束

1. 除非任务明确变更兼容性，否则保持对外可见的 Kong 行为不变。
2. 代理与核心请求路径保持 Rust 原生。
3. 使用直接 SQL/sqlx 与既有 schema/DAO 模式；不引入 ORM。
4. 兼容层能解决的问题，不要修改上游 Kong Lua 插件源码。
5. 数据库迁移保持 forward-only、按序注册，并有迁移/schema 测试覆盖。

## 权威命令

除非另有说明，命令在仓库根目录执行。

| 用途 | 命令 |
| --- | --- |
| 构建 | `make build` |
| Release 构建 | `make release` |
| 快速工作区检查 | `make check` |
| 全部测试（默认 PostgreSQL） | `make test` |
| PostgreSQL 测试（受管依赖 + 破坏性清理） | `make test-pg` |
| DB-less 测试 | `make test-dbless` |
| 格式化 | `make fmt` |
| 格式化检查 | `make fmt-check` |
| Clippy | `make lint` |
| 启动 PostgreSQL + 迁移 + 服务 | `make dev` |
| 启动 DB-less 服务 | `make dev-dbless` |
| 启动受管依赖 | `make services-up` |
| 停止依赖并删除受管卷 | `make services-down` |
| 安装 Manager 依赖 | `make manager-install` |
| 构建 Manager | `make manager-build` |
| 运行 Manager | `make manager-dev` |
| 构建容器镜像 | `make docker-build` |
| 运行/停止 DB-less 镜像 | `make docker-run` / `make docker-stop` |
| 以 PostgreSQL 运行镜像 | `make docker-run-pg` |

`make test` 委托给 `scripts/run-cargo-test.sh`，将
`KONG_TEST_*`/`KONG_SPEC_TEST_*` 变量映射为生效的 `KONG_*` 变量，并使用
`cargo test --locked`。

Manager 专用命令在 `kong-manager/` 目录下执行：

```bash
pnpm lint
pnpm build
pnpm test:e2e
```

Docker 目标接受 `DOCKER_REGISTRY`、`DOCKER_VERSION` 和 `DOCKER_PLATFORM`；
例如构建本地 arm64 镜像使用 `DOCKER_PLATFORM=linux/arm64 make docker-build`。

## 本地运行时

`make dev` 启动 Docker PostgreSQL、执行 `db bootstrap`、应用 `db up`，然后
启动 Kong-Rust。默认本地端点：

- Proxy: `http://127.0.0.1:8000`
- Admin API: `http://127.0.0.1:8001`
- Kong Manager: `http://127.0.0.1:8002`
- Status API: `http://127.0.0.1:8007`

依赖脚本可能为 PostgreSQL 分配动态宿主端口。使用其导出的环境变量，不要
假设端口是 5432。

`make services-down` 会执行 `docker compose down -v --remove-orphans` 并
删除受管 PostgreSQL 卷。`make test-pg` 在测试结束后执行相同清理。需要保留
本地依赖数据时，不要使用这两个命令。

## 验证

选择能证明改动的最小测试，再按风险扩大范围：

- 单个 Rust crate：`cargo check -p <crate>` 及其聚焦测试。
- 跨 crate / 运行时装配：`cargo check -p kong-server` 加受影响的测试。
- 数据库/schema 变更：迁移注册测试、DAO/schema 测试，可行时跑一次真实
  PostgreSQL 升级。
- 代理变更：聚焦的单元/集成测试；行为对外可见时通过 8000 端口发真实请求
  验证。
- Manager 变更：`pnpm lint`、`pnpm build`，对变更的流程做基于浏览器的 UI
  验证。
- 大范围变更：`make check`、`make lint` 及相关 `make test*` 目标。

始终运行 `git diff --check`。若仓库级格式化检查暴露的是既有失败，不要
重排无关文件；报告基线并保持自己负责的 diff 干净。

## 文档

持久知识库：

- `docs/pm/backlog.md` — 需求单、交付门禁与状态。
- `docs/requirements.md` — 范围与需求。
- `docs/design.md` — 架构与组件契约。
- `docs/tasks.md` — 跟踪的实现工作与状态。
- `docs/implementation-logs/` — 重要实现记录。
- `docs/ai-gateway-guide.md` 与 `_cn.md` — AI Gateway 使用指南。
- `docs/codex-agent-migration.md` — Claude Code 到 Codex 的迁移说明。

对实质性功能或缺陷修复：

1. 跟踪状态或范围变化时更新 `docs/tasks.md`。
2. 改动引入持久行为、API、迁移或架构时，新增或更新 implementation log。
3. 架构/接口变更时更新 `docs/design.md`。
4. 仅当产品范围变化时更新 `docs/requirements.md`。

不要为琐碎的格式化或纯文档编辑制造虚假进度记录。

## Codex 工作流

- Codex 自动读取本 `AGENTS.md`；不要依赖 `CLAUDE.md`。
- 模糊或高风险工作使用 Plan 模式；独立的仓库调查或验证使用有边界的
  子代理。
- 本地 UI 工作使用可用的应用内浏览器技能并验证可见的用户流程，不要依赖
  Claude-in-Chrome 工具。
- 可复用的仓库工作流放在 `.agents/skills/<name>/SKILL.md`。技能保持窄
  聚焦，不要把生成的依赖或模型 SDK 收入仓库。
- 个人的模型、审批、沙箱和 MCP 偏好放在 `~/.codex/config.toml`；只有
  全团队刻意共享的设置才放入项目 `.codex/config.toml`。

## AI 提示词与模型变更

- Agent 指令保持精炼：每条规则只说一次，优先仓库事实与成功标准而非过程
  叙述，避免"think harder"、"逐步推理"之类的泛化指令。
- 提示词模板需定义目标、相关上下文、硬约束、审批边界、所需证据、成功
  标准和输出契约。只保留编码了产品需求或修复了实测失败的示例。
- 将模型、推理力度、端点、工具、缓存和多模态细节视为彼此独立的兼容性
  决策。绝不全局替换模型名。
- 迁移模型时保持工作负载角色：旗舰、均衡、高吞吐路由可能需要不同的目标
  模型。除非任务明确包含，否则保持历史示例、fixture、tokenizer 映射、
  provider 兼容用例和有意设计的降级不变。
- 新的 OpenAI 推理、工具调用和多轮行为优先使用 Responses API。保留 Chat
  Completions 时验证其模型/工具兼容性，而不是悄悄改变推理配置或移除工具。
- 一次只改一个提示词关注点，并用有代表性的任务验证。先比较任务成功率和
  所需证据，再看 token、延迟和成本；不接受更短更便宜但不满足输出契约的
  结果。
- 稳定可复用的提示词前缀不含请求特定值。采用显式缓存控制或模型特定请求
  字段前，先实测缓存行为。

## 完成定义

当所请求的行为已实现、相关检查通过、diff 中无无关变更、必需的文档已更新、
且剩余告警或未验证风险被明确报告时，任务才算完成。
