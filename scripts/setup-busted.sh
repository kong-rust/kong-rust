#!/usr/bin/env bash
# scripts/setup-busted.sh
# 安装 busted 测试框架及其依赖 — Install busted test framework and dependencies

set -euo pipefail

echo "=== 安装 luarocks (如果未安装) ==="
if ! command -v luarocks &>/dev/null; then
    if command -v brew &>/dev/null; then
        brew install luarocks
    else
        echo "错误: 请先安装 luarocks (brew install luarocks 或从 https://luarocks.org 下载)"
        exit 1
    fi
fi

echo "=== 安装 busted 测试框架 ==="
luarocks install --local --lua-version=5.1 busted 2.2.0-1

echo "=== 安装 luasocket (HTTP 客户端) ==="
luarocks install --local --lua-version=5.1 luasocket 3.1.0-1

echo "=== 安装 luasec (HTTPS 支持) ==="
luarocks install --local --lua-version=5.1 luasec

echo "=== 安装 lua-cjson (JSON 编解码) ==="
luarocks install --local --lua-version=5.1 lua-cjson 2.1.0.10-1

echo "=== 安装 luafilesystem (文件系统操作) ==="
luarocks install --local --lua-version=5.1 luafilesystem

echo "=== 安装 penlight (工具库) ==="
luarocks install --local --lua-version=5.1 penlight

echo "=== 验证安装 ==="
eval "$(luarocks path --bin)"
busted --version

echo "=== 安装完成 ==="
echo "运行 'eval \"\$(luarocks path --bin)\"' 或将其加入 shell profile"
