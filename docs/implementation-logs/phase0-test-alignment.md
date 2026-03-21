# Phase 0: Kong 官方测试对齐 — 实现日志

## 概述

使 Kong-Rust 能够运行 Kong 官方 spec 测试文件，验证 Admin API 和代理层与 Kong 原版的行为兼容性。

## 测试结果汇总

| Spec 文件 | 初始 | 最终 | 通过率 | 状态 |
|-----------|------|------|--------|------|
| 00-smoke (all) | 59/59 | **59/59** | **100%** | ✅ |
| 04-admin_api/10-services_routes | 12/75 | **75/75** | **100%** | ✅ |
| 04-admin_api/03-consumers_routes | 17/99 | **99/99** | **100%** | ✅ |
| 04-admin_api/04-plugins_routes | 18/26 | **25/26** | **96%** | 1 schema |
| 04-admin_api/14-tags | 5/14 | **14/14** | **100%** | ✅ |
| 04-admin_api/02-kong_routes | 27/45 | **40/45** | **89%** | 4 dbless + 1 restart |
| 05-proxy/33-request-id-header | 2/10 | **10/10** | **100%** | ✅ |
| 05-proxy/08-uri_encoding | 0/5 | **5/5** | **100%** | ✅ |
| **总计** | **140/333** | **327/333** | **98.2%** | **+187** |

## 已完成的工作

### Task 1-3: 基础设施（已完成）
- Mock upstream server（Rust axum 实现，端口 15555/15556）
- spec.helpers 兼容层（1600+ 行）
- 断言系统对齐（assert.response/request/status/header/jsonbody 等）

### Task 4: Admin API Spec 对齐（进行中）

#### 复制的 spec 文件（P0 + P1）
| 文件 | 行数 | 状态 |
|------|------|------|
| 01-admin_api_spec.lua | 84 | 需要适配（依赖 nginx 配置） |
| 02-kong_routes_spec.lua | 800+ | 需要运行 |
| 03-consumers_routes_spec.lua | 1000+ | 需要运行 |
| 04-plugins_routes_spec.lua | 600+ | 需要运行 |
| 09-routes_routes_spec.lua | 2000+ | 需要运行 |
| 10-services_routes_spec.lua | 850 | **44/75 通过 (59%)** |
| 14-tags_spec.lua | 300+ | 需要运行 |
| 06-certificates_routes_spec.lua | 1400+ | 需要运行 |
| 07-upstreams_routes_spec.lua | 1000+ | 需要运行 |
| 08-targets_routes_spec.lua | 1200+ | 需要运行 |
| 16-ca_certificates_routes_spec.lua | 350+ | 需要运行 |

#### 创建的兼容层
1. **Kong Lua shim 模块**（新建）
   - `kong/tools/uuid.lua` — UUID v4 生成和验证
   - `kong/tools/string.lua` — 字符串工具（strip, split, validate_utf8）
   - `kong/tools/table.lua` — 表工具（nkeys, deep_copy）
   - `kong/db/errors.lua` — DB 错误码和工厂方法
   - `kong/meta.lua` — 版本元数据
   - `kong/constants.lua` — 核心常量（HEADERS, PROTOCOLS, BUNDLED_PLUGINS）
   - `table/nkeys.lua` — 独立 nkeys shim
   - `resty/http.lua` — lua-resty-http 兼容层（基于 luasocket）

2. **Admin API Rust 端修复**
   - `FlexibleBody` 提取器：支持 JSON + form-urlencoded + multipart Content-Type
   - 移除 `skip_serializing_if = "Option::is_none"`：响应包含 null 字段
   - 修复 URL shorthand 展开：正确处理 `"/"` 路径
   - 分页响应：仅在有值时包含 offset/next
   - DELETE 幂等：不存在的资源也返回 204

3. **helpers.lua 增强**
   - `ngx` 全局变量 shim（ngx.null, ngx.escape_uri 等）
   - `DbProxy` 对象：通过 Admin API 模拟直接 DB 访问
   - Blueprint 命名实体生成器（named_services, key_auth_plugins 等）
   - HTTP 客户端 cjson.null 处理

#### 剩余的 10-services_routes_spec.lua 失败（31 个）
- 空 URL / 无效 URL 验证（应返回 400，实际返回 201）
- 分页 next URL 缺少 size 参数
- form-urlencoded 中 cjson.null 语义不一致
- 部分 PATCH 更新的字段值比较问题

### Task 5: Proxy Spec 对齐（进行中）
- 复制了 P0/P1 proxy spec 文件（8 个）
- 01-proxy_spec.lua 依赖 nginx 配置，需要适配
- 创建了 resty.http shim 模块

### Task 6-7: DB / Status API / DBless Spec
- 文件已复制，待运行

### Task 8: 插件 Spec
- 待 Task 4-5 稳定后开始

### Task 9: CI 集成（已完成）
- 创建 `.github/workflows/spec-tests.yml`
- 包含 PostgreSQL service、busted 安装、smoke test、admin API spec

### Task 10: 文档更新（已完成）
- 创建本实现日志

## 关键发现

1. Kong spec 文件大量依赖 OpenResty 生态（resty.http, ngx.*），需要创建 shim 层
2. Kong 的 `it_content_types` 模式测试三种 Content-Type，必须在 Rust 端支持
3. Kong 的 Blueprint 系统有大量命名实体生成器（named_services 等），需要逐一实现
4. 分页和错误响应格式的细微差异是兼容性的主要挑战
