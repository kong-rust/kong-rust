#!/usr/bin/env bash

# 依赖服务管理核心脚本
# 用法: common.sh <env_file> up|down

if [ "$#" -lt 2 ]; then
    echo "用法: $0 <env_file> up|down"
    exit 1
fi

cwd=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# 检测 docker compose 命令
if docker compose version >/dev/null 2>&1; then
    DOCKER_COMPOSE="docker compose"
elif command -v docker-compose >/dev/null 2>&1; then
    DOCKER_COMPOSE="docker-compose"
else
    echo "错误: 未安装 docker compose 或 docker-compose"
    exit 1
fi

export COMPOSE_FILE="$cwd/docker-compose-test-services.yml"
export COMPOSE_PROJECT_NAME="kong-rust-dev"

KONG_SERVICE_ENV_FILE=$1

if [ "$2" == "down" ]; then
    $DOCKER_COMPOSE down -v --remove-orphans
    exit 0
fi

# 清空环境变量文件
> "$KONG_SERVICE_ENV_FILE"

echo "export COMPOSE_FILE=$COMPOSE_FILE" >> "$KONG_SERVICE_ENV_FILE"
echo "export COMPOSE_PROJECT_NAME=$COMPOSE_PROJECT_NAME" >> "$KONG_SERVICE_ENV_FILE"

# 启动服务并等待健康检查
$DOCKER_COMPOSE up -d --wait --remove-orphans

if [ $? -ne 0 ]; then
    echo "错误: 服务启动失败，请检查 docker compose 输出"
    exit 1
fi

# 提取动态映射的端口并导出环境变量
# 格式: "服务名 环境变量名 容器内端口"（每行一个服务）
_extract_port() {
    local svc=$1 env_name=$2 private_port=$3

    local exposed_port
    exposed_port=$($DOCKER_COMPOSE port "$svc" "$private_port" 2>/dev/null | cut -d: -f2)

    if [ -z "$exposed_port" ]; then
        echo "警告: 无法获取 $svc 的端口 $private_port"
        return
    fi

    for prefix in KONG_ KONG_TEST_ KONG_SPEC_TEST_; do
        echo "export ${prefix}${env_name}=$exposed_port" >> "$KONG_SERVICE_ENV_FILE"
        echo "export ${prefix}$(echo "$svc" | tr '[:lower:]-' '[:upper:]_')_HOST=127.0.0.1" >> "$KONG_SERVICE_ENV_FILE"
    done
}

_extract_port postgres PG_PORT 5432
