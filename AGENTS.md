# Kong-Rust Agent Guide

This file is the repository-level source of truth for coding agents. It applies
to the whole repository unless a closer `AGENTS.md` provides more specific
instructions.

## Working Agreement

- Preserve unrelated user changes in dirty worktrees.
- Prefer small, reviewable diffs. Do not run repository-wide formatters unless
  the task explicitly calls for it and the resulting churn is reviewed.
- Use `rg` and `rg --files` for repository searches.
- Reuse existing modules, helpers, schemas, and test patterns before adding new
  abstractions or dependencies.
- Never commit credentials, generated secrets, local databases, logs, or agent
  runtime state.
- Treat references to Anthropic or Claude models in AI Gateway code and docs as
  product compatibility, not as legacy Claude Code configuration.

## Project Map

Kong-Rust is a Rust 2021 rewrite of Kong Gateway built on Pingora. The workspace
is currently verified with Rust 1.94.

```text
crates/
├── kong-core            Core models and traits
├── kong-config          kong.conf parsing
├── kong-db              PostgreSQL DAO, cache, DB-less mode, migrations
├── kong-router          Traditional and expressions routers
├── kong-proxy           Pingora HTTP/stream proxy
├── kong-plugin-system   Plugin registry and phase execution
├── kong-lua-bridge      Lua compatibility layer and PDK
├── kong-admin           Axum Admin API
├── kong-cluster         CP/DP clustering
├── kong-ai              AI Gateway providers, codecs, routing, accounting
└── kong-server          Main binary and runtime wiring
```

Other important paths:

- `kong-manager/` — Vue 3 + TypeScript management UI.
- `scripts/` — test runners and dependency-service orchestration.
- `spec/` — Kong-compatible Lua/spec tests.
- `docs/` — requirements, design, task tracking, guides, and implementation logs.

Dependencies must remain one-way from lower-level crates toward `kong-server`.
Avoid circular dependencies.

## Design Constraints

1. Preserve externally visible Kong behavior unless a task explicitly changes
   compatibility.
2. Keep the proxy and core request path Rust-native.
3. Use direct SQL/sqlx and the existing schema/DAO patterns; do not introduce an
   ORM.
4. Avoid modifying upstream Kong Lua plugin sources when the compatibility
   layer can solve the problem.
5. Keep database migrations forward-only, registered in order, and covered by
   migration/schema tests.

## Authoritative Commands

Run commands from the repository root unless noted otherwise.

| Purpose | Command |
| --- | --- |
| Build | `make build` |
| Release build | `make release` |
| Fast workspace check | `make check` |
| All tests, PostgreSQL default | `make test` |
| PostgreSQL tests with managed dependency and destructive teardown | `make test-pg` |
| DB-less tests | `make test-dbless` |
| Formatting | `make fmt` |
| Formatting check | `make fmt-check` |
| Clippy | `make lint` |
| Start PostgreSQL + migrate + server | `make dev` |
| Start DB-less server | `make dev-dbless` |
| Start managed dependencies | `make services-up` |
| Stop dependencies and delete managed volumes | `make services-down` |
| Install Manager dependencies | `make manager-install` |
| Build Manager | `make manager-build` |
| Run Manager | `make manager-dev` |
| Build container image | `make docker-build` |
| Run/stop DB-less image | `make docker-run` / `make docker-stop` |
| Run image with PostgreSQL | `make docker-run-pg` |

`make test` delegates to `scripts/run-cargo-test.sh`, maps
`KONG_TEST_*`/`KONG_SPEC_TEST_*` variables to effective `KONG_*` variables, and
uses `cargo test --locked`.

Manager-only commands run from `kong-manager/`:

```bash
pnpm lint
pnpm build
pnpm test:e2e
```

Docker targets accept `DOCKER_REGISTRY`, `DOCKER_VERSION`, and
`DOCKER_PLATFORM`; for example, use
`DOCKER_PLATFORM=linux/arm64 make docker-build` for a local arm64 image.

## Local Runtime

`make dev` starts Docker PostgreSQL, runs `db bootstrap`, applies `db up`, and
starts Kong-Rust. Default local endpoints are:

- Proxy: `http://127.0.0.1:8000`
- Admin API: `http://127.0.0.1:8001`
- Kong Manager: `http://127.0.0.1:8002`
- Status API: `http://127.0.0.1:8007`

The dependency script may assign PostgreSQL a dynamic host port. Use its
exported environment rather than assuming port 5432.

`make services-down` runs `docker compose down -v --remove-orphans` and deletes
the managed PostgreSQL volume. `make test-pg` performs the same teardown after
the test run. Do not use either command when local dependency data must be
preserved.

## Verification

Choose the smallest test that proves the change, then broaden according to
risk:

- Single Rust crate: `cargo check -p <crate>` and its focused tests.
- Cross-crate/runtime wiring: `cargo check -p kong-server` plus affected tests.
- Database/schema changes: migration registration tests, DAO/schema tests, and
  a real PostgreSQL upgrade when practical.
- Proxy changes: focused unit/integration tests and a real request through port
  8000 when behavior is externally visible.
- Manager changes: `pnpm lint`, `pnpm build`, and browser-based UI validation
  for changed flows.
- Broad changes: `make check`, `make lint`, and the relevant `make test*`
  target.

Always run `git diff --check`. If a repository-wide formatting check exposes
pre-existing failures, do not reformat unrelated files; report the baseline and
keep the owned diff clean.

## Documentation

The durable knowledge base is:

- `docs/requirements.md` — scope and requirements.
- `docs/design.md` — architecture and component contracts.
- `docs/tasks.md` — tracked implementation work and status.
- `docs/implementation-logs/` — substantial implementation records.
- `docs/ai-gateway-guide.md` and `_cn.md` — AI Gateway usage.
- `docs/codex-agent-migration.md` — Claude Code to Codex transition notes.

For substantive features or bug fixes:

1. Update `docs/tasks.md` when tracked status or scope changes.
2. Add or update an implementation log when the change introduces durable
   behavior, APIs, migrations, or architecture.
3. Update `docs/design.md` for architecture/interface changes.
4. Update `docs/requirements.md` only when product scope changes.

Do not create fake progress entries for trivial formatting or documentation-only
edits.

## Codex Workflow

- Codex reads this `AGENTS.md` automatically; do not depend on `CLAUDE.md`.
- Use Plan mode for ambiguous or high-risk work and bounded subagents for
  independent repository investigation or verification.
- For local UI work, use the available in-app browser skill and verify the
  visible user flow. Do not depend on Claude-in-Chrome tools.
- Reusable repository workflows belong in `.agents/skills/<name>/SKILL.md`.
  Keep skills narrow and do not vendor generated dependencies or model SDKs
  into the repository.
- Personal model, approval, sandbox, and MCP preferences belong in
  `~/.codex/config.toml`; add project `.codex/config.toml` only for settings the
  whole team intentionally shares.

## Definition of Done

A task is complete when the requested behavior is implemented, relevant checks
pass, the diff contains no unrelated changes, required documentation is
updated, and remaining warnings or unverified risks are reported explicitly.
