# Kong-Rust 开发命令
# ==================

# ---------- 构建 ----------

# 编译整个 workspace
build:
	cargo build --workspace

# Release 构建
release:
	cargo build --workspace --release

# 快速类型检查（不生成二进制，比 build 快）
check:
	cargo check --workspace

# 编译单个 crate（用法: make crate=kong-core build-crate）
build-crate:
	cargo build -p $(crate)

# ---------- 测试 ----------

# 运行所有测试
test:
	cargo test --workspace

# 运行单个 crate 的测试
# 用法: make crate=kong-router test-crate
test-crate:
	cargo test -p $(crate)

# 运行匹配名称的测试
# 用法: make name=test_route_match test-name
test-name:
	cargo test --workspace $(name) -- --nocapture

# 运行测试并显示输出（不捕获 stdout/stderr）
test-verbose:
	cargo test --workspace -- --nocapture

# 只运行集成测试
test-integration:
	cargo test --workspace --test '*'

# ---------- 启动 / 调试 ----------

# 启动服务（默认配置）
run:
	cargo run -p kong-server --bin kong

# 指定配置文件启动
# 用法: make conf=/path/to/kong.conf run-conf
run-conf:
	cargo run -p kong-server --bin kong -- -c $(conf)

# Debug 模式启动（详细日志）
run-debug:
	RUST_LOG=debug cargo run -p kong-server --bin kong

# Trace 级别日志（最详细）
run-trace:
	RUST_LOG=trace cargo run -p kong-server --bin kong

# 指定单个模块的日志级别
# 用法: make mod=kong_router run-mod-debug
run-mod-debug:
	RUST_LOG=warn,$(mod)=debug cargo run -p kong-server --bin kong

# ---------- 代码质量 ----------

# 格式化
fmt:
	cargo fmt --all

# 格式检查（CI 用）
fmt-check:
	cargo fmt --all -- --check

# Clippy 静态分析
lint:
	cargo clippy --workspace -- -D warnings

# 格式化 + lint 一起跑
quality: fmt lint

# ---------- Kong Manager GUI ----------

MANAGER_DIR = kong-manager
MANAGER_PORT ?= 8002
ADMIN_API ?= http://127.0.0.1:8001

# 安装 kong-manager 前端依赖
manager-install:
	cd $(MANAGER_DIR) && pnpm install

# 构建 kong-manager 静态文件（输出到 kong-manager/dist/）
manager-build:
	cd $(MANAGER_DIR) && pnpm build

# 开发模式启动 kong-manager（热更新，默认 8080 端口）
# Admin API 地址通过 ADMIN_API 变量配置
# 用法: make manager-dev
#        make ADMIN_API=http://10.0.0.1:8001 manager-dev
manager-dev:
	cd $(MANAGER_DIR) && KONG_GUI_URL=$(ADMIN_API) pnpm serve

# 预览生产构建（先 build 再 serve）
manager-preview:
	cd $(MANAGER_DIR) && pnpm preview

# ---------- 依赖服务管理 ----------

SERVICES_DIR = scripts/dependency_services

# 启动依赖服务（PostgreSQL 等）
services-up:
	@bash $(SERVICES_DIR)/common.sh /dev/null up

# 停止依赖服务并清理数据卷
services-down:
	@bash $(SERVICES_DIR)/common.sh /dev/null down

# 查看服务日志
services-logs:
	@COMPOSE_FILE=$(SERVICES_DIR)/docker-compose-test-services.yml \
		COMPOSE_PROJECT_NAME=kong-rust-dev \
		docker compose logs -f

# ---------- 全栈启动 ----------

# 一键启动：依赖服务 → db bootstrap → cargo run（postgres 模式）
# 端口由 docker 动态分配，通过环境变量传递
dev:
	@export KONG_SERVICE_ENV_FILE=$$(mktemp); \
		bash $(SERVICES_DIR)/common.sh $$KONG_SERVICE_ENV_FILE up && \
		. $$KONG_SERVICE_ENV_FILE && \
		echo "PostgreSQL 端口: $$KONG_PG_PORT" && \
		KONG_PG_PORT=$$KONG_PG_PORT RUST_LOG=info cargo run -p kong-server --bin kong -- -c kong.conf.default db bootstrap; \
		. $$KONG_SERVICE_ENV_FILE && \
		KONG_PG_PORT=$$KONG_PG_PORT RUST_LOG=info cargo run -p kong-server --bin kong -- -c kong.conf.default; \
		rm -f $$KONG_SERVICE_ENV_FILE

# db-less 模式，无需 docker
dev-dbless:
	KONG_DATABASE=off RUST_LOG=info cargo run -p kong-server --bin kong

# 同时启动 kong-server（后台）+ kong-manager（前台）
dev-full:
	@echo "启动 kong-server (后台)..."
	@RUST_LOG=info cargo run -p kong-server --bin kong &
	@sleep 2
	@echo "启动 kong-manager (前台, http://localhost:$(MANAGER_PORT))..."
	@cd $(MANAGER_DIR) && KONG_GUI_URL=$(ADMIN_API) pnpm serve

# ---------- Docker ----------

DOCKER_TAG ?= kong-rust:latest
DOCKER_REGISTRY ?=
DOCKER_PLATFORM ?= linux/amd64

# 构建 Docker 镜像（默认 linux/amd64，适配 x86 服务器）
docker-build:
	docker buildx build --platform $(DOCKER_PLATFORM) -t $(DOCKER_TAG) --load .

# 推送 Docker 镜像
docker-push:
	docker push $(DOCKER_REGISTRY)$(DOCKER_TAG)

# db-less 模式运行容器
docker-run:
	docker run -d --name kong-rust \
		-e KONG_DATABASE=off \
		-p 8000:8000 -p 8443:8443 \
		-p 8001:8001 -p 8444:8444 \
		$(DOCKER_TAG)

# PostgreSQL 模式运行容器
# 用法: make KONG_PG_HOST=host KONG_PG_PASSWORD=pass docker-run-pg
docker-run-pg:
	docker run -d --name kong-rust \
		-e KONG_DATABASE=postgres \
		-e KONG_PG_HOST=$(KONG_PG_HOST) \
		-e KONG_PG_PORT=$(KONG_PG_PORT) \
		-e KONG_PG_USER=$(KONG_PG_USER) \
		-e KONG_PG_PASSWORD=$(KONG_PG_PASSWORD) \
		-e KONG_PG_DATABASE=$(KONG_PG_DATABASE) \
		-p 8000:8000 -p 8443:8443 \
		-p 8001:8001 -p 8444:8444 \
		$(DOCKER_TAG)

# 停止并删除容器
docker-stop:
	docker rm -f kong-rust 2>/dev/null || true

# ---------- 清理 ----------

# 清理 Rust 构建产物
clean:
	cargo clean

# 清理 kong-manager 构建产物
manager-clean:
	rm -rf $(MANAGER_DIR)/node_modules $(MANAGER_DIR)/dist

# 清理所有
clean-all: clean manager-clean

# ---------- 信息 ----------

# 查看依赖树
deps:
	cargo tree --workspace

# 查看 workspace 成员
members:
	@cargo metadata --format-version 1 --no-deps | python3 -c \
		"import sys,json; [print(p['name']) for p in json.load(sys.stdin)['packages']]"

.PHONY: build release check build-crate \
        test test-crate test-name test-verbose test-integration \
        run run-conf run-debug run-trace run-mod-debug \
        fmt fmt-check lint quality \
        manager-install manager-build manager-dev manager-preview \
        services-up services-down services-logs \
        dev dev-dbless dev-full \
        docker-build docker-push docker-run docker-run-pg docker-stop \
        clean manager-clean clean-all deps members
