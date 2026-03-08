# ==========================================
# Stage 1: 构建阶段 — Build stage
# ==========================================
FROM rust:1.88-bookworm AS builder

# 系统构建依赖 — System build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl-dev \
    libpq-dev \
    libluajit-5.1-dev \
    pkg-config \
    build-essential \
    cmake \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# 依赖层缓存：先复制 Cargo 配置文件 — Dependency layer cache: copy Cargo configs first
COPY Cargo.toml Cargo.lock ./
COPY crates/kong-core/Cargo.toml crates/kong-core/Cargo.toml
COPY crates/kong-config/Cargo.toml crates/kong-config/Cargo.toml
COPY crates/kong-db/Cargo.toml crates/kong-db/Cargo.toml
COPY crates/kong-router/Cargo.toml crates/kong-router/Cargo.toml
COPY crates/kong-proxy/Cargo.toml crates/kong-proxy/Cargo.toml
COPY crates/kong-plugin-system/Cargo.toml crates/kong-plugin-system/Cargo.toml
COPY crates/kong-lua-bridge/Cargo.toml crates/kong-lua-bridge/Cargo.toml
COPY crates/kong-admin/Cargo.toml crates/kong-admin/Cargo.toml
COPY crates/kong-server/Cargo.toml crates/kong-server/Cargo.toml

# 创建占位源文件，使 cargo 能解析依赖并编译 — Create dummy source files for dependency compilation
RUN mkdir -p crates/kong-core/src && echo "" > crates/kong-core/src/lib.rs && \
    mkdir -p crates/kong-config/src && echo "" > crates/kong-config/src/lib.rs && \
    mkdir -p crates/kong-db/src && echo "" > crates/kong-db/src/lib.rs && \
    mkdir -p crates/kong-router/src && echo "" > crates/kong-router/src/lib.rs && \
    mkdir -p crates/kong-proxy/src && echo "" > crates/kong-proxy/src/lib.rs && \
    mkdir -p crates/kong-plugin-system/src && echo "" > crates/kong-plugin-system/src/lib.rs && \
    mkdir -p crates/kong-lua-bridge/src && echo "" > crates/kong-lua-bridge/src/lib.rs && \
    mkdir -p crates/kong-admin/src && echo "" > crates/kong-admin/src/lib.rs && \
    mkdir -p crates/kong-server/src && echo "fn main() {}" > crates/kong-server/src/main.rs

# 编译依赖（这一层会被缓存，除非 Cargo.toml/lock 变化）— Compile dependencies (cached unless Cargo files change)
RUN cargo build --release --workspace 2>/dev/null || true

# 复制真正的源码 — Copy real source code
COPY crates/ crates/

# 清除占位文件的编译缓存，触发增量编译 — Invalidate dummy build artifacts for incremental rebuild
RUN find target/release/.fingerprint -name "kong-*" -exec rm -rf {} + 2>/dev/null || true

# 编译最终二进制 — Build final binary
RUN cargo build --release -p kong-server

# ==========================================
# Stage 2: 运行时阶段 — Runtime stage
# ==========================================
FROM debian:bookworm-slim

# 运行时依赖 — Runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl3 \
    libpq5 \
    libluajit-5.1-2 \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# 创建 kong 用户和标准目录 — Create kong user and standard directories
RUN groupadd -r kong && useradd -r -g kong kong && \
    mkdir -p /etc/kong /usr/local/kong && \
    chown -R kong:kong /etc/kong /usr/local/kong

# 复制二进制和入口脚本 — Copy binary and entrypoint
COPY --from=builder /build/target/release/kong /usr/local/bin/kong
COPY docker-entrypoint.sh /docker-entrypoint.sh
RUN chmod +x /docker-entrypoint.sh

# 复制默认配置文件（如果存在）— Copy default config (if exists)
COPY kong.conf.default /etc/kong/kong.conf.default

USER kong

# Kong 标准端口 — Kong standard ports
# 8000: Proxy HTTP
# 8443: Proxy HTTPS
# 8001: Admin API HTTP
# 8444: Admin API HTTPS
EXPOSE 8000 8443 8001 8444

STOPSIGNAL SIGQUIT

HEALTHCHECK --interval=60s --timeout=10s --retries=10 \
    CMD kong health || exit 1

ENTRYPOINT ["/docker-entrypoint.sh"]
CMD ["kong", "docker-start"]
