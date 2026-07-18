# Task 15.3: 面向 AI Endpoint 的无 JSON 管理交互

## 目标

让第一次使用 AI Gateway 的用户在一个页面内发布、验证和维护可调用的
OpenAI-compatible Chat Endpoint，无需理解 Service、Route、Plugin、Model Group
之间的关系，也无需填写 JSON。

## 实现

- 将主菜单中的 AI Gateway 设为左侧第一个入口，并把 `/ai-gateway` 默认入口改为
  AI Endpoint 聚合列表。Endpoint 是现有 Provider、Model、Service、Route 和
  route-scoped `ai-proxy` Plugin 的投影，不新增运行时实体或第二份配置。
- 单页表单分为接口信息、模型池、流量策略和发布摘要。Provider 类型、API Key、
  自定义服务地址、真实模型名和流量权重均使用结构化控件；Providers 和 Advanced
  Models 的常规 CRUD 也移除了 JSON 编辑器。
- `useEndpointPublisher` 使用版本化 tag 识别资源所有权，按 Provider → Model →
  Service → Route → Plugin 的顺序创建，并在失败时逆序回滚。更新会替换受管 Model
  并修补 Service、Route 和 Plugin；删除保留可复用 Provider。
- Endpoint 列表根据底层实体重建名称、路径、模型池、启用状态和完整性；缺少成员时
  显示需要处理，不静默补建资源。
- 测试台通过 `POST /ai-endpoint-test` 调用本机 Proxy listener。Admin helper 只接受
  `/ai/{slug}/v1/chat/completions` 形态、且数据库中存在受管 tag Route 的请求，避免
  浏览器跨端口 CORS，同时不允许任意 URL 或直接 Provider 转发。
- 语言开关移到 Manager 顶部，所有路由页面复用全局 locale。首次访问按浏览器语言
  选择中文或英文，显式选择写入 local storage，并同步 HTML `lang`；AI Gateway
  保留领域词典，但不再维护独立语言状态。
- 左上角品牌更新为 **Kong Rust Manager**，右上角 GitHub 链接指向
  `kong-rust/kong-rust`。Overview 中的 Kong Konnect 推广卡片以及对应组件、词典
  和图片素材已删除。
- OpenAI-compatible driver 在 Provider 只填写服务根地址时自动补齐
  `/v1/chat/completions`；完整自定义路径保持不变。这样向导中填写
  `https://api.deepseek.com` 或本地 Ollama 根地址即可直接转发，不会请求上游 `/`
  而得到 404。
- 模型流量策略不再要求权重总和等于 100。单模型权重限制为 `0..=10000`，
  Model Group 和 `model_routes` 均使用交错加权轮转，避免 50:50 等配置在低流量时
  先连续命中同一模型；`model_routes` 路由器按 Route 和配置缓存，使轮转状态跨请求
  保留。Admin API 与 PostgreSQL 约束同步拒绝越界权重。

## 验证

- `cargo check -p kong-admin --locked`
- `cargo test -p kong-admin ai_endpoint_test --lib --locked`
- `pnpm lint:vue`
- `pnpm lint:eslint`（仅项目原有 2 个 warning）
- `pnpm lint:style`
- `pnpm lint:playwright`
- `pnpm build`
- AI Gateway Playwright：Endpoint 创建、读取、更新、真实代理调用、内置测试台和
  删除，并断言向导生成的 Service 与 `ai-proxy` Plugin 默认启用；Provider、
  Advanced Model、Virtual Key 的 CRUD；一次性密钥轮换；菜单顺序；
  全局中英文切换、实体页面语言持久化和中文浏览器默认值。4 个用例全部通过。
- Manager 主实体回归运行 206 条 Playwright 用例，其中 91 条通过；7 条失败均为
  当前 Admin API/插件集合与上游 Kong Manager 固有预期不一致（非法 Route、Service、
  Target、Vault 校验，Key/Upstream 兼容行为），其余 108 条因串行 describe 在失败后
  被跳过。失败页面快照确认新品牌、菜单和语言状态正常；测试资源随后全部清理。
- 使用临时 DeepSeek Provider 和 `deepseek-v4-flash` 模型，经
  `POST /ai/deepseek-forwarding-validation/v1/chat/completions` 实际转发，返回
  HTTP 200 和 `OK`；随后删除 Endpoint、Model 和 Provider。测试 API Key 未写入
  代码、文档或测试夹具。

## 当前边界

- 首版只创建 OpenAI Chat Completions 客户端协议的 Route。
- 跨 Provider 失败重试和健康 fallback 仍需等待运行时失败回报闭环完成。
- 内置测试台的流式请求会经过 Proxy，但 Admin helper 首版缓冲完整响应后再展示；
  curl 示例仍直接指向公开 Proxy Endpoint，可用于观察实时 SSE。
- Endpoint 删除默认保留 Provider，因为连接可能被其他 Endpoint 或高级资源复用。
