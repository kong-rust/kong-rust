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
	cargo run -p kong-server

# 指定配置文件启动
# 用法: make conf=/path/to/kong.conf run-conf
run-conf:
	cargo run -p kong-server -- -c $(conf)

# Debug 模式启动（详细日志）
run-debug:
	RUST_LOG=debug cargo run -p kong-server

# Trace 级别日志（最详细）
run-trace:
	RUST_LOG=trace cargo run -p kong-server

# 指定单个模块的日志级别
# 用法: make mod=kong_router run-mod-debug
run-mod-debug:
	RUST_LOG=warn,$(mod)=debug cargo run -p kong-server

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

# ---------- 全栈启动 ----------

# 同时启动 kong-server（后台）+ kong-manager（前台）
# 用法: make dev
dev:
	@echo "启动 kong-server (后台)..."
	@RUST_LOG=info cargo run -p kong-server &
	@sleep 2
	@echo "启动 kong-manager (前台, http://localhost:$(MANAGER_PORT))..."
	@cd $(MANAGER_DIR) && KONG_GUI_URL=$(ADMIN_API) pnpm serve

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
        dev clean manager-clean clean-all deps members
