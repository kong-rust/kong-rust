# 阶段 19A:AI Tokenizer — 精确 prompt-token 计数

**日期**:2026-04-30
**任务**:19A.1 - 19A.5(全部完成)
**目的**:为 AI Gateway 添加精确的 prompt token 预扣计数能力,替代 ai-rate-limit 中粗糙的 `body.len()/4` 估算,并为 balancer 引入 `by_token_size` 路由策略。

## 背景与动机

### Background and Motivation

之前 `ai-rate-limit` 插件在 access 阶段预扣 TPM 用的是 `request_body.len() / 4` 字节估算 — 误差在 ±50% 量级,常导致 TPM 限流过松或过紧。
`tiktoken-rs` 已被引入但 `TokenCounter` 在 src/ 中是孤立件,只有测试在用。
balancer (`ModelGroupBalancer`) 没有按 prompt 大小路由的能力,无法做"短 prompt 走小模型省钱、长 prompt 自动升档大模型"。

The previous `ai-rate-limit` access-stage pre-debit used `request_body.len() / 4` byte estimation — within ±50% error, frequently mis-throttling TPM. `tiktoken-rs` was already wired but `TokenCounter` was unused in src/. The balancer had no by-prompt-size routing.

## 核心设计 / Core Design

### 优先级链(用户最终规则)

```
任何模型:
  has_non_text ?
    true  → 远端 count API(OpenAI/Anthropic/Gemini)→ 失败回 HF → tiktoken → 估算
    false → HF 本地编码(Xenova/gpt-4o 等)→ 失败回 tiktoken → 估算
```

不再把 OpenAI 当作特殊 provider,统一逻辑:**任何模型只要有 HF tokenizer 资源,本地编码主路径就是 HF**。tiktoken-rs **只作最终兜底**。

### Per-request 整体 deadline 300ms,远端单次 timeout 1s

`TokenizerRegistry::count_prompt` 用 `tokio::time::timeout(per_request_deadline, ...)` 包外层;远端 HTTP 客户端各自再设 1s timeout 作为下一层兜底。

### LRU 缓存(三家共享)

`moka::sync::Cache`,key = `(provider: &'static str, model: String, has_non_text: bool, prompt_sha256: [u8; 32])`,容量 1024 / TTL 60s。本地 tokenizer(tiktoken / HF)不走缓存(已经够快)。

LRU key 强制包含 `has_non_text` 维度 — 同 prompt 文本不同上下文(纯文本 vs 多模态)token 数不同,不能串扰。

### HF 单飞 + 首次降级不阻塞

`HfLoader` 用 DashMap + AtomicBool CAS 实现单飞:首次 cache miss 时,第一个调用方 spawn 后台下载(tokio::spawn),后续并发调用 see Pending → 立即降级返回 None;下载完成后所有后续调用同步命中 Loaded。

测试中用 MockHfDownloader + 100 并发验证只触发一次 download。

### OpenAI HF 内置 Xenova mapping

```
gpt-4o*  → Xenova/gpt-4o
gpt-4*   → Xenova/gpt-4
gpt-3.5* → Xenova/gpt-3.5-turbo
o1/o3/o4 → None(返回 None,让 tiktoken 兜底)
```

用户配置 mapping 优先,然后内置 Xenova,然后 model 含 `/` 直接当 repo_id。

### HF 多模态先只算文本(TODO 留待后续)

`extract_prompt_text` 在 array content 中只取 `type=text` 的 part,自动忽略 `image_url`/`input_audio`/`input_file`。`HfTokenizer::count_prompt` 加 `TODO(multimodal)` 注释说明后续需要 vision tower patch token 计算。

## 修改文件清单

### 新增

- `crates/kong-ai/src/token/tokenizer.rs`(343 行)— PromptTokenizer trait + 五个实现 + `extract_prompt_text` + `has_non_text_content` + `openai_default_xenova_repo`
- `crates/kong-ai/src/token/registry.rs`(495 行)— TokenizerRegistry + 路由策略 + 全局单例 + `from_kong_config` 转换
- `crates/kong-ai/src/token/hf_loader.rs`(297 行)— HfLoader 状态机 + HfDownloader trait + HttpHfDownloader + 单飞 + 原子写入
- `crates/kong-ai/src/token/remote_count.rs`(440 行)— RemoteCountClient trait + RemoteCountKey + RemoteCountCache + 三个 HTTP client + chat→responses 转换
- `crates/kong-ai/tests/tokenizer_registry_test.rs`(30 测试)
- `crates/kong-ai/tests/hf_loader_test.rs`(11 测试)
- `crates/kong-ai/tests/remote_count_test.rs`(18 测试)

### 修改

- `Cargo.toml`(workspace) — 加 `tokenizers = "0.23"`
- `crates/kong-ai/Cargo.toml` — 把 `reqwest` 提到主 deps、加 `tokenizers` / `moka`
- `crates/kong-ai/src/token/mod.rs` — 导出新模块
- `crates/kong-ai/src/models.rs::AiModel` — 加 `max_input_tokens: Option<i32>`
- `crates/kong-ai/src/provider/balancer.rs::ModelGroupBalancer` — 加 `select_for(prompt_tokens)` + `fits_token_budget`
- `crates/kong-ai/src/plugins/context.rs::AiRequestState` — 加 `estimated_prompt_tokens: u64`
- `crates/kong-ai/src/plugins/ai_proxy.rs` — pass-through 和常规两分支都接入 registry,写入 `AiRequestState.estimated_prompt_tokens`
- `crates/kong-ai/src/plugins/ai_rate_limit.rs` — `compute_estimated_prompt_tokens` helper(三级降级:state > registry > byte/4)
- `crates/kong-ai/tests/balancer_test.rs` — 加 8 个 by_token_size 测试
- `crates/kong-config/src/config.rs::KongConfig` — 加 13 个 `ai_tokenizer_*` 字段 + 默认值 + set 解析
- `crates/kong-server/src/main.rs::build_plugin_registry` — 启动时根据 KongConfig 构造并 set_global_registry

## 代码统计

- 新增源代码 ~1575 行(不含测试)
- 新增测试 ~1100 行(67 个新测试)
- workspace 全量回归:0 fail

## 配置参考(kong.conf)

```ini
# ============ AI Tokenizer ============
ai_tokenizer_enabled = true
ai_tokenizer_per_request_deadline_ms = 300
ai_tokenizer_remote_count_timeout_ms = 1000
ai_tokenizer_cache_capacity = 1024
ai_tokenizer_cache_ttl_seconds = 60
ai_tokenizer_offline = false

# HF tokenizer.json 缓存目录(可选,默认 $HOME/.cache/kong-rust/tokenizers)
# ai_tokenizer_cache_dir = /var/lib/kong/tokenizers

# OpenAI 远端 count 端点(可选,默认 https://api.openai.com)
# ai_tokenizer_openai_endpoint = https://api.openai.com
# ai_tokenizer_openai_api_key = sk-...

# Anthropic 远端 count 端点(可选,默认 https://api.anthropic.com)
# ai_tokenizer_anthropic_endpoint = https://api.anthropic.com
# ai_tokenizer_anthropic_api_key = sk-ant-...

# Gemini 远端 count 端点(可选,默认 https://generativelanguage.googleapis.com)
# ai_tokenizer_gemini_endpoint = https://generativelanguage.googleapis.com
# ai_tokenizer_gemini_api_key = ...
```

## 测试 / Tests

```bash
cargo test -p kong-ai          # 全套 280+ 测试,0 fail
cargo test --workspace         # 全 workspace,0 fail
```

关键测试场景覆盖:

- **OpenAI 双轨**:纯文本不调远端(只走 HF/tiktoken);含 image_url/tools 优先调远端;远端失败 → HF cache miss → tiktoken 兜底
- **真实 HTTP body 解析**(axum mock server):
  - OpenAI Bearer auth + image_url 数组结构透传 + tools 透传 + LRU 命中
  - Anthropic system 提到顶层 + `x-api-key` + `anthropic-version: 2023-06-01`
  - Gemini `assistant→model` 角色映射 + `systemInstruction` + URL `?key=`
- **LRU key 区分 has_non_text**:同 prompt 不同 has_non_text 不串(服务端被调 2 次)
- **HF 单飞**:100 并发请求,downloader 只调用 1 次
- **首次降级**:cache miss 立即返回字符估算,后台下载完成后第二次精确
- **balancer by_token_size**:同 priority 全过滤 → fallback 下一档 + 边界条件

## 已知限制 / Known Limitations

- HF 多模态(image_url / input_audio)token 暂时只算文本部分;TODO 留待 vision tower patch 计算
- OpenAI o1/o3/o4 系列暂无 Xenova HF port,直接走 tiktoken-rs(精度等同)
- 远端 API key 缺失时 client 不构造,行为退化到本地路径(OpenAI HF/tiktoken;Anthropic/Gemini 字符估算)

## 后续迭代方向 / Follow-ups

- 多模态精确 token 计算(LLaVA/Qwen-VL/InternVL 公式各异)
- 动态 mapping 配置:kong.conf 支持 `ai_tokenizer_mapping_<n>` 数组(目前只能代码注入)
- HF 离线包:打包预下载常用模型 tokenizer.json 到 image

## PR

- https://github.com/kong-rust/kong-rust/pull/17 (8 commits: 5 feat + 3 fix)
