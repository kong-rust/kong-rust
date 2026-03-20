# Task 13.2: 修复 WebSocket 代理握手头转发

**Date:** 2026-03-20
**Status:** Completed

## Summary

修复 WebSocket 代理握手失败问题。原实现仅透传 Upgrade 和 Connection 头，丢失了 Sec-WebSocket-Key、Sec-WebSocket-Version、Sec-WebSocket-Protocol、Sec-WebSocket-Extensions 等握手必需头，导致上游服务器无法完成 WebSocket 握手。

## Root Cause

`upstream_request_filter` 中的 WebSocket 处理块只硬编码设置了 `upgrade: websocket` 和 `connection: upgrade`，没有从原始请求中转发 `sec-websocket-*` 系列头。

## Fix

在检测到 WebSocket upgrade 后，遍历原始请求头中所有以 `sec-websocket-` 开头的头并转发到上游请求。

## Files Modified

- `crates/kong-proxy/src/lib.rs:995-1025` — WebSocket 握手头转发逻辑

## Artifacts

### Functions
- `upstream_request_filter` (WebSocket section, line ~995-1025) — 检测 upgrade: websocket 后，设置 Upgrade/Connection 头并转发所有 sec-websocket-* 握手头

## Statistics
- Lines added: 15
- Lines removed: 3
