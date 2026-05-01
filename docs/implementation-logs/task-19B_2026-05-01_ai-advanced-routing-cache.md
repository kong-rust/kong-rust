# 阶段 19B:AI 高级路由 + 语义缓存 — 实现日志

**日期:** 2026-05-01
**任务:** 19B.1 / 19B.2 / 19B.3
**分支:** `claude/ai-advanced-routing-cache`(基于 main 的 `117378c`)

## 背景

Kong Manager AI 页面突出三大能力,但 kong-rust 后端尚未完整实现:
1. 按 input token 数路由 — `balancer.rs::select_for(prompt_tokens)` 已就位(PR #17),但 ai-proxy 未接通
2. ai-semantic-cache 插件 — 完全缺失,只有字符串精确匹配的 ai-cache
3. 语义路由 — 完全缺失

## 提交清单

### Commit 1 — `feat(ai): wire token-size routing into ai-proxy`

**修改文件**:
- `crates/kong-ai/src/provider/router.rs`(+121/-25):`ModelTargetConfig` 三个新字段、`ModelRouter::resolve_for(model_name, prompt_tokens)` 新方法、`fits_token_budget` helper
- `crates/kong-ai/src/plugins/ai_proxy.rs`(+71/-15):`AiProxyConfig.enable_token_size_routing`、access 阶段 token 估算前置、X-Kong-AI-Selected-Model / X-Kong-AI-Prompt-Tokens 响应头
- `crates/kong-ai/tests/router_test.rs`(+219/-2):helpers 默认值补齐 + 9 个新测试

**统计**:3 文件,+428/-42 行

### Commit 2 — `feat(ai): ai-semantic-cache plugin with in-memory vector store`

**新增文件**:
- `crates/kong-ai/src/embedding/mod.rs`(99 行):`EmbeddingClient` trait + 余弦相似度工具 + 6 个测试
- `crates/kong-ai/src/embedding/openai.rs`(132 行):`OpenAiEmbeddingClient`(POST /v1/embeddings,支持 OpenAI/Azure/vLLM/Ollama,可定制 auth header)
- `crates/kong-ai/src/embedding/vector_store.rs`(220 行):`VectorStore` trait + `InMemoryVectorStore`(LRU + 惰性 TTL)+ 6 个测试
- `crates/kong-ai/src/plugins/ai_semantic_cache.rs`(363 行):插件实现 + cache_key 提取(LastMessage / AllMessages / FirstUserMessage)+ 6 个单测
- `crates/kong-ai/tests/ai_semantic_cache_test.rs`(282 行):9 个端到端集成测试

**修改文件**:
- `crates/kong-ai/src/lib.rs`:注册 `embedding` 模块
- `crates/kong-ai/src/plugins/mod.rs`:导出 `AiSemanticCachePlugin`
- `crates/kong-server/src/main.rs`:注册 `ai-semantic-cache` 插件

**统计**:8 文件,+1317/-0 行

### Commit 3 — `feat(ai): semantic routing based on prompt embedding similarity`

**修改文件**:
- `crates/kong-ai/src/provider/router.rs`(+47):`candidates_for_priority` + `build_resolution_at` 两个辅助方法
- `crates/kong-ai/src/plugins/ai_proxy.rs`(+220):`AiProxyConfig` 6 个新字段、`AiProxyPlugin.semantic_indices` DashMap、`SemanticRoutingIndex` 结构、`semantic_index_for(cfg)` 惰性预热、`semantic_config_hash` 哈希、`pick_semantic_target` 选择函数、access 阶段语义路由分支
- `crates/kong-ai/tests/ai_proxy_semantic_routing_test.rs`(263 行):5 个端到端测试

**统计**:3 文件,+530/-9 行

## 设计决策

### Token-size routing 复用 model_routes 而不是 DAO AiModel

`balancer.rs::ModelGroupBalancer` 已实现 `select_for(prompt_tokens)`,但它消费的是数据库 `AiModel` 列表;ai-proxy 实际用的是 plugin config 里的 `model_routes`(`ModelTargetConfig` 列表,基于正则匹配)。两套系统并存。

**决策**:不绕道 DAO,直接给 `ModelTargetConfig` 加 `priority` + `max_input_tokens`,让 `ModelRouter::resolve_for` 自己实现 priority 分组 + 过滤 + fallback。这样配置仍然集中在 plugin config,无 DB seeding 也能用。

**代价**:`ModelRouter` 是 stateless 的(每请求 `from_configs(...)`),所以 cooldown 状态不存在(若需要将来再做 plugin-instance 缓存)。

### token 估算用启发式 provider,不是路由后才知道的真实 provider

`TokenizerRegistry::count_prompt(provider_type, model, request)` 需要 provider 信息。但 router 决议之前我们还不知道实际路由到哪个 provider(routing 决策本身就依赖 token 数)。

**决策**:用 `TokenizerRegistry::infer_provider_type(model_name)` 启发式推断(已有方法,基于 model 名前缀)。token 估值只用于"过滤候选",几百到几万 token 的差距粗略计算够用;路由后下游记录还会用真实 provider 重新计数。

### ai-semantic-cache vector_store = InMemory only(任务作者倾向)

任务里"我倾向是 InMemory only"。Redis 后端实现作为阶段 19B.4 follow-up,trait 已就位,任何非 InMemory 值 fallback InMemory + warn。

### Plugin-instance per-config 缓存

`AiSemanticCachePlugin` 和 `AiProxyPlugin`(语义路由)都用 `DashMap<config_hash, Arc<...>>` 模式持有 per-route 状态(vector store / examples 索引)。`config_hash` 只覆盖"影响构造的字段",这样 timeout/threshold 这种 runtime 字段改动不会触发昂贵的重建。

### Semantic routing 叠加在 token-size 之上

执行顺序:**正则匹配 → token-size 过滤 → priority 选档 → 语义选最高分 → fallback 加权 RR**。语义路由先依赖 `candidates_for_priority` 拿到过滤后的高 priority 档候选集,再在该集合里按 cosine 选。

### Embedding 调用同步阻塞 + 短 deadline

任务作者倾向"MVP 同步即可,加 deadline 200ms"。embedding 在 access 阶段同步阻塞调用,timeout 在 reqwest 层强制(默认 200ms)。失败 → 降级路径(语义路由 fallback 加权 RR;语义缓存跳过缓存查找)。

## 测试覆盖

| 测试文件 | 测试数 | 覆盖场景 |
|---------|--------|---------|
| `router_test.rs`(增量) | 9 | by-token-size routing 全场景 + JSON 反序列化 |
| `embedding/mod.rs::tests` | 6 | 余弦相似度数学正确性 |
| `embedding/vector_store.rs::tests` | 6 | TTL / LRU / threshold / KNN 选最高分 |
| `ai_semantic_cache.rs::tests` | 6 | cache key 提取 + config hash |
| `ai_semantic_cache_test.rs` | 9 | 插件全生命周期 |
| `ai_proxy_semantic_routing_test.rs` | 5 | 三领域路由 + 双 fallback 路径 |
| **总计新增** | **41** | |

`cargo test -p kong-ai`:**398 passed / 0 failed**(之前 357 + 新增 41)

`cargo check --workspace`:0 errors(仅 pre-existing warnings)

## 可观测性契约

新增/确认 6 个稳定响应头:

| Header | 出现条件 | 含义 |
|--------|---------|-----|
| `X-Kong-AI-Selected-Model` | 任何走 model_routes 的请求 | 实际路由的上游 model |
| `X-Kong-AI-Prompt-Tokens` | enable_token_size_routing=true | 路由决策的 token 估值 |
| `X-Kong-AI-Cache: HIT-SEMANTIC` | ai-semantic-cache 命中 | 语义缓存命中 |
| `X-Kong-AI-Cache: MISS-SEMANTIC` | ai-semantic-cache 未命中 | 已写回 |
| `X-Kong-AI-Cache-Similarity: 0.xxxx` | ai-semantic-cache 命中 | 余弦分数 |
| `X-AI-Skip-Cache: 1`(请求头) | 客户端发送 | 跳过 ai-semantic-cache |

## 已知限制 / Follow-up

1. **Redis 后端**:trait 已就位,实现待补(19B.4)
2. **Embedding provider 仅 OpenAI 兼容**:Anthropic/Gemini 没有标准 embeddings 端点;Cohere/Voyage 等需要后续扩展 trait 实现
3. **vector store brute-force O(N)**:适合 ≤10k 条目;更大规模建议接 Redis + RediSearch / Pinecone / Qdrant 等向量数据库
4. **cooldown 不在 ModelRouter 路径**:`ModelGroupBalancer` 的 cooldown / 健康追踪能力没接进 ai-proxy(因为 router stateless)
