#!/usr/bin/env bash
# scripts/run-specs.sh
# Run Kong spec tests — 运行 Kong spec 测试
#
# Usage:
#   ./scripts/run-specs.sh                    # run all specs
#   ./scripts/run-specs.sh spec/00-smoke/     # run specific directory
#   ./scripts/run-specs.sh spec/00-smoke/01-admin_api_spec.lua  # run specific file

set -euo pipefail

SPEC_PATH="${1:-spec/}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Ensure busted is available — 确保 busted 可用
eval "$(luarocks path --bin 2>/dev/null)" || true
if ! command -v busted &>/dev/null; then
    echo "错误: busted 未安装。运行: make setup-busted"
    exit 1
fi

# Build kong (skip if binary exists and is newer than source) — 编译 kong（如果二进制已存在且较新则跳过）
if [ ! -f "${ROOT}/target/debug/kong" ]; then
    echo "=== 编译 kong ==="
    cargo build --quiet 2>&1 || cargo build 2>&1
fi

# Set up Lua paths — 设置 Lua 路径
export LUA_PATH="${ROOT}/spec/?.lua;${ROOT}/spec/?/init.lua;${ROOT}/?.lua;${ROOT}/?/?.lua;${ROOT}/?.lua;${ROOT}/?/init.lua;${LUA_PATH:-}"
export KONG_RUST_BIN="${ROOT}/target/debug/kong"

# Run specs — 运行测试
echo "=== 运行 Kong spec: ${SPEC_PATH} ==="
cd "${ROOT}"
busted \
    --helper=spec/helpers.lua \
    -o utfTerminal \
    --no-auto-insulate \
    -v \
    "${SPEC_PATH}"
