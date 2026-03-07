#!/usr/bin/env bash

# 启动依赖服务并导出环境变量
# 必须通过 source 调用: source scripts/dependency_services/up.sh

if [ "${BASH_SOURCE-}" = "$0" ]; then
    echo "必须用 source 调用此脚本: source $0" >&2
    exit 33
fi

export KONG_SERVICE_ENV_FILE=$(mktemp)

if [ -n "$ZSH_VERSION" ]; then
    cwd=$(dirname $(readlink -f ${(%):-%N}))
else
    cwd=$(dirname $(readlink -f ${BASH_SOURCE[0]}))
fi

/usr/bin/env bash "$cwd/common.sh" "$KONG_SERVICE_ENV_FILE" up
if [ $? -ne 0 ]; then
    echo "错误: 服务启动失败，请检查 common.sh 输出"
    return 1
fi

. "$KONG_SERVICE_ENV_FILE"

stop_services () {
    if test -n "$COMPOSE_FILE" && test -n "$COMPOSE_PROJECT_NAME"; then
        bash "$cwd/common.sh" "$KONG_SERVICE_ENV_FILE" down
    fi

    for i in $(cat "$KONG_SERVICE_ENV_FILE" | cut -f2 | cut -d '=' -f1); do
        unset "$i"
    done

    rm -rf "$KONG_SERVICE_ENV_FILE"
    unset KONG_SERVICE_ENV_FILE
    unset -f stop_services
}

echo '服务已启动! 使用 "stop_services" 停止服务并清理环境变量。'
