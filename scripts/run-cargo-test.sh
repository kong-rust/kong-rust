#!/usr/bin/env bash

set -euo pipefail

# Map KONG_TEST_* / KONG_SPEC_TEST_* to effective KONG_* variables before running cargo tests.
# 在运行 cargo test 前，将 KONG_TEST_* / KONG_SPEC_TEST_* 映射为实际生效的 KONG_* 变量。

print_usage() {
    cat <<'EOF'
Usage: scripts/run-cargo-test.sh [--print-effective-env] [cargo test args...]

Examples:
  scripts/run-cargo-test.sh --workspace
  KONG_TEST_DATABASE=off scripts/run-cargo-test.sh --workspace
  KONG_TEST_DATABASE=postgres KONG_TEST_PG_PORT=55432 scripts/run-cargo-test.sh -p kong-admin
EOF
}

apply_prefix_overrides() {
    local prefix=$1
    while IFS='=' read -r key value; do
        [[ $key == ${prefix}* ]] || continue
        local target_key="KONG_${key#${prefix}}"
        export "${target_key}=${value}"
    done < <(env)
}

print_effective_env() {
    env | sort | grep -E '^KONG_' || true
}

main() {
    local print_env=false
    local -a cargo_args=()

    while (($#)); do
        case "$1" in
            --print-effective-env)
                print_env=true
                shift
                ;;
            --help|-h)
                print_usage
                exit 0
                ;;
            *)
                cargo_args+=("$1")
                shift
                ;;
        esac
    done

    # Apply generic test overrides first, then more specific spec-test overrides.
    # 先应用通用测试覆盖，再应用更具体的 spec-test 覆盖。
    apply_prefix_overrides "KONG_TEST_"
    apply_prefix_overrides "KONG_SPEC_TEST_"

    # Match Kong's default test strategy: postgres unless explicitly overridden.
    # 与 Kong 默认测试策略保持一致：除非显式覆盖，否则默认使用 postgres。
    export KONG_DATABASE="${KONG_DATABASE:-postgres}"

    if [[ "${print_env}" == "true" ]]; then
        print_effective_env
        exit 0
    fi

    if ((${#cargo_args[@]} == 0)); then
        cargo_args=(--workspace)
    fi

    exec cargo test --locked "${cargo_args[@]}"
}

main "$@"
