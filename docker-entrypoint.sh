#!/usr/bin/env bash
set -Eeo pipefail

# file_env — 支持 Docker Secrets
# 用法: file_env 'KONG_PG_PASSWORD' 'default_value'
# 如果 KONG_PG_PASSWORD_FILE 存在，从文件读取值设置到 KONG_PG_PASSWORD
file_env() {
    local var="$1"
    local fileVar="${var}_FILE"
    local def="${2:-}"
    if [ "${!var:-}" ] && [ "${!fileVar:-}" ]; then
        echo >&2 "error: both $var and $fileVar are set (but are exclusive)"
        exit 1
    fi
    local val="$def"
    if [ "${!var:-}" ]; then
        val="${!var}"
    elif [ "${!fileVar:-}" ]; then
        val="$(< "${!fileVar}")"
    fi
    export "$var"="$val"
    unset "$fileVar"
}

# 处理常见的 Docker Secrets 环境变量
file_env 'KONG_PG_PASSWORD'
file_env 'KONG_PG_USER'
file_env 'KONG_PG_DATABASE'
file_env 'KONG_PG_HOST'

# 容器日志默认值：错误日志仅 stderr，访问日志到 stdout
export KONG_PROXY_ERROR_LOG="${KONG_PROXY_ERROR_LOG:-off}"
export KONG_PROXY_ACCESS_LOG="${KONG_PROXY_ACCESS_LOG:-/dev/stdout}"

# 确保 /usr/local/kong 目录可写（前缀目录）
if [ ! -w /usr/local/kong ]; then
    mkdir -p /usr/local/kong
fi

# 如果第一个参数不是 kong 命令，直接执行
if [ "$1" != "kong" ]; then
    exec "$@"
fi

# 跳过 "kong" 本身，传递子命令给 kong 二进制
shift
exec kong "$@"
