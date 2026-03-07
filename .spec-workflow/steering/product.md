# 产品概述

## 产品目标

Kong-Rust 是使用 Rust 语言和 Cloudflare Pingora 框架**完全重写** Kong API 网关的项目。最终目标是**零成本替换** Kong —— 无需修改任何现有 Kong 配置和 Lua 插件，所有数据模型、API 接口和使用习惯与 Kong 保持完全一致。

## 目标用户

- **Kong 运维工程师**：希望用更高性能、更低资源占用的方案替换现有 Kong 部署，且不修改任何配置
- **API 网关开发者**：希望在 Rust 生态中构建高性能 API 网关，同时复用 Kong 庞大的 Lua 插件生态
- **平台团队**：需要在 CP/DP 分离架构下独立扩缩管理平面和数据平面

## 核心特性

1. **100% 兼容 Kong**：数据模型、Admin API、配置格式（kong.conf）、声明式配置（YAML/JSON）、Lua 插件接口完全一致
2. **高性能代理**：基于 Pingora 的多线程共享连接池、Rust 原生路由引擎
3. **Lua 插件兼容**：通过 mlua + LuaJIT 运行 Kong 全部 47 个内置 Lua 插件，提供完整 PDK 和 ngx.* 兼容层
4. **Hybrid 模式**：支持 Control Plane / Data Plane 分离部署，含全量推送（Sync V1）和增量同步（Sync V2, JSON-RPC 2.0）
5. **多种数据源**：PostgreSQL 数据库模式和 db-less 声明式配置模式

## 业务目标

- 实现从 Kong 的**无感迁移**（使用 decK dump → Kong-Rust import 即可切换）
- 单节点吞吐量显著优于原版 Kong（Rust + Pingora vs LuaJIT + OpenResty）
- 内存安全（Rust 所有权系统），消除 Kong 中偶发的内存泄漏问题
- 支持所有 Kong 部署模式：单节点、Hybrid CP/DP、db-less

## 成功标准

- **兼容性**：Kong 官方测试用例子集（spec/）通过率 ≥ 95%
- **性能**：P99 延迟不高于 Kong，吞吐量至少持平
- **插件兼容**：47 个内置 Lua 插件全部可加载运行
- **迁移验证**：decK 导出配置可直接导入并正常代理

## 产品原则

1. **兼容优先**：所有外部行为与 Kong 完全一致，不引入 Kong 不存在的行为
2. **Rust 原生**：核心路径（路由、代理、负载均衡）用 Rust 实现，追求极致性能
3. **渐进替换**：按模块逐步实现，每个阶段都可独立测试和验证
4. **最小侵入**：不修改 Kong 的 Lua 插件代码，不改变数据库 Schema

## 未来演进

- **原生 Rust 插件**：为高频插件（key-auth、rate-limiting）提供 Rust 原生实现，进一步提升性能
- **gRPC/WebSocket 代理增强**：利用 Pingora 原生支持优化非 HTTP 协议代理
- **Kubernetes Ingress Controller**：适配 Kong Ingress Controller 接口
- **可观测性增强**：原生 OpenTelemetry 集成、Prometheus 指标导出
