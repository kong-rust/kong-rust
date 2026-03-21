# Phase 0 Step 1: busted + spec.helpers 核心兼容层

## 实现概要

搭建了 Kong 官方 spec 测试兼容框架，使 Kong 官方 spec 文件可以直接在 Kong-Rust 上运行。

## 架构

- 进程级集成测试：Kong-Rust 作为子进程启动，busted CLI 执行 spec 文件
- spec/helpers.lua：兼容层，提供 start_kong/stop_kong/proxy_client/admin_client/Blueprint 等 API
- HTTP 客户端：luasocket（替代 resty.http，不依赖 openresty）
- Fixture 创建：通过 Admin API（不直连数据库）

## 修改文件

| 操作 | 文件 |
|------|------|
| 新增 | `scripts/setup-busted.sh` |
| 新增 | `spec/helpers.lua` |
| 新增 | `spec/kong_tests.conf` |
| 新增 | `spec/fixtures/http_client.lua` |
| 新增 | `spec/00-smoke/01-admin_api_spec.lua` |
| 新增 | `scripts/run-specs.sh` |
| 新增 | `crates/kong-server/tests/spec_runner.rs` |
| 修改 | `Makefile` |

## 代码统计

- 新增文件：7
- 新增代码：~600 行 Lua + ~80 行 Rust + ~40 行 Shell

## 测试结果

烟雾测试 5/5 通过：
- GET / 返回节点信息
- GET /status 返回状态信息
- Service CRUD 完整流程
- Route + Service 关联创建
- Blueprint 通过 Admin API 创建 fixture
