# Kong-Rust

A Rust-native **AI Gateway** — API Gateway, LLM Gateway, Agent Gateway, and MCP/Skill Gateway in a single binary. 100% compatible with [Kong Gateway](https://github.com/Kong/kong) in API gateway scenarios — same features, better performance, drop-in replacement.

## Why Kong-Rust?

The AI era needs a new kind of gateway. Traditional API gateways handle HTTP traffic; LLM proxies handle model calls; MCP gateways route tool access — but none of them cover the full picture. Kong-Rust unifies all four gateway types into **one Rust-native AI Gateway**, built on [Cloudflare Pingora](https://github.com/cloudflare/pingora).

```
┌─────────────────────────────────────────────────┐
│              Kong-Rust  AI Gateway              │
│                                                 │
│  ┌───────────┐ ┌───────────┐ ┌───────────────┐ │
│  │API Gateway│ │LLM Gateway│ │ Agent Gateway  │ │
│  │(Kong 100%)│ │           │ │               │ │
│  └───────────┘ └───────────┘ └───────────────┘ │
│  ┌──────────────────────────────────────────┐   │
│  │         MCP / Skill Gateway              │   │
│  └──────────────────────────────────────────┘   │
│                                                 │
│  Rust · Pingora · Single Binary                 │
└─────────────────────────────────────────────────┘
```

| | Kong (Lua) | LiteLLM (Python) | Kong-Rust |
|---|---|---|---|
| **API Gateway** | Full | None | **Full (100% Kong compatible, faster)** |
| **LLM Gateway** | Lua plugins | Full (100+ providers) | Rust-native (roadmap) |
| **Agent Gateway** | None | None | Rust-native (roadmap) |
| **MCP / Skill Gateway** | Enterprise | Basic | Rust-native (roadmap) |
| **Engine** | OpenResty (Nginx + LuaJIT) | uvicorn | **Pingora (Rust, multi-threaded)** |
| **Language** | Lua | Python | **Rust** |

## Features

### API Gateway (Kong Compatible — Faster & Stronger)

Everything Kong does, Kong-Rust does — with Rust-level performance and memory safety on top.

- **100% Kong Compatibility** — Same data models, Admin API, `kong.conf` format, declarative config (YAML/JSON), and Lua plugin interface (PDK + `ngx.*`). Existing Kong deployments can migrate with zero config changes.
- **Superior Performance** — Pingora's multi-threaded architecture replaces OpenResty's single-threaded event loop. Shared connection pools, zero-copy proxying, no GC pauses. True multi-core utilization without worker process overhead.
- **Memory Safety** — Rust ownership system eliminates use-after-free, buffer overflows, and data races that plague C/Lua FFI boundaries.
- **Dual Routing Engine** — Both `traditional_compatible` and `expressions` router flavors, with LRU route cache for hot-path acceleration
- **Full Lua Plugin Ecosystem** — Run all 47 built-in Kong Lua plugins via mlua + LuaJIT — no plugin rewrites needed
- **Load Balancing & Health Checks** — Round-robin, consistent-hashing, least-connections, latency-based. Active/passive health checks with automatic recovery.
- **TLS Termination & SNI** — Certificate management with SNI-based routing, HTTP/2 ALPN, upstream mTLS
- **L4 Stream Proxy** — TCP/TLS passthrough proxy with SNI-based and source/destination CIDR routing
- **Kong Manager UI** — Works with the official Kong Manager frontend
- **Multiple Data Sources** — PostgreSQL or db-less (declarative config) modes
- **Hybrid Mode** — Control Plane / Data Plane separation (planned)

### LLM Gateway (Roadmap)

- **Token-based Rate Limiting** — TPM/RPM per key/route/consumer
- **Multi-model Load Balancing & Fallback** — Multiple LLM providers as upstreams, auto-failover
- **Virtual API Key Management** — Issue virtual keys mapped to real provider keys with budgets
- **Token Cost Tracking** — Per key/team/route usage and cost metrics
- **Semantic Caching** — Vector-similarity cached LLM responses
- **Prompt Guard** — Regex + semantic prompt injection detection

### Agent Gateway (Roadmap)

- **Agent Communication Routing** — Route and manage inter-agent traffic
- **Agent Identity & Access Control** — Per-agent authentication and authorization
- **Agent Observability** — Latency, error rate, and usage metrics per agent

### MCP / Skill Gateway (Roadmap)

- **MCP Server Registry** — Register, discover, and version MCP servers via Admin API
- **MCP Routing & Load Balancing** — Route tool calls to MCP servers with failover
- **Skill Orchestration** — Skill registration, composition, and execution
- **Auth & Observability** — Per-tool/per-agent access control, call metrics

### AI Gateway Console (Roadmap)

Enterprise-grade management console replacing Kong Manager OSS. Built with React 19 + Next.js 15 + shadcn/ui.

- **Unified Dashboard** — Four sub-gateway health status, traffic trends, cost overview
- **LLM Cost Dashboard** — Real-time/historical token spend, drill-down by key/team/route
- **Virtual API Key Management** — Issue keys, bind providers, set budgets and quotas
- **Fallback Chain Editor** — Visual drag-and-drop multi-model failover configuration
- **Agent Topology** — Visualize inter-agent communication and call chains
- **MCP Tool Tracing** — End-to-end trace from Agent → MCP → Tool

## Architecture

```
kong-server (binary entry point)
 ├── kong-core          — Core data models and traits
 ├── kong-config        — Configuration parser (kong.conf format)
 ├── kong-db            — PostgreSQL DAO + cache + db-less + migrations
 ├── kong-router        — Routing engine (traditional + expressions + L4 stream)
 ├── kong-proxy         — Pingora proxy engine (L7 HTTP + L4 stream) + load balancing + health checks
 ├── kong-plugin-system — Plugin registry and execution framework
 ├── kong-lua-bridge    — Lua compatibility layer + PDK + ngx.*
 ├── kong-admin         — Admin API (axum)
 ├── kong-cluster       — CP/DP cluster communication (planned)
 ├── kong-ai            — LLM Gateway engine: rate limiter, OpenAI/Anthropic protocol, token counting (planned)
 ├── kong-mcp           — MCP/Skill Gateway: MCP protocol, tool registry & routing (planned)
 └── kong-agent         — Agent Gateway: A2A protocol, agent registry & routing (planned)
```

## Quick Start

### Prerequisites

- Rust 1.94.0+ (with Cargo)
- PostgreSQL 15+ (for database mode), or none (for db-less mode)
- Docker (optional, for managed PostgreSQL)

### Database Mode

```bash
# Start PostgreSQL (via Docker)
make services-up

# One-command start: PostgreSQL + DB bootstrap + run
make dev
```

### DB-less Mode

```bash
# No database required
make dev-dbless
```

### Manual Setup

```bash
# Build
cargo build --workspace

# Initialize database
cargo run -p kong-server -- -c kong.conf.default db bootstrap

# Start
cargo run -p kong-server -- -c kong.conf.default
```

### Verify

```bash
# Admin API
curl http://localhost:8001/

# Create a service
curl -X POST http://localhost:8001/services \
  -H 'Content-Type: application/json' \
  -d '{"name":"httpbin","url":"https://httpbin.org"}'

# Create a route
curl -X POST http://localhost:8001/services/httpbin/routes \
  -H 'Content-Type: application/json' \
  -d '{"paths":["/httpbin"]}'

# Proxy through the gateway
curl http://localhost:8000/httpbin/get
```

## Kong Manager

Kong-Rust is compatible with the official [Kong Manager](https://docs.konghq.com/gateway/latest/kong-manager/) frontend.

`8001` is the Admin API port. `8002` is the Kong Manager GUI port. Admin endpoints such as `/services` are served on `8001`, not `8002`.

```bash
# Install dependencies
make manager-install

# Start in development mode (default: http://localhost:8002)
make manager-dev
```

## Configuration

Kong-Rust uses the same `kong.conf` format as Kong. See `kong.conf.default` for all available options.

Key settings:

| Setting | Default | Description |
|---------|---------|-------------|
| `proxy_listen` | `0.0.0.0:8000` | HTTP proxy listener address |
| `admin_listen` | `127.0.0.1:8001` | Admin API listener address |
| `stream_listen` | `off` | L4 stream proxy listener (e.g., `0.0.0.0:9000`) |
| `database` | `postgres` | Database mode (`postgres` or `off`) |
| `pg_host` | `127.0.0.1` | PostgreSQL host |
| `pg_port` | `5432` | PostgreSQL port |
| `pg_database` | `kong` | PostgreSQL database name |
| `router_flavor` | `traditional_compatible` | Router engine (`traditional_compatible` or `expressions`) |

Environment variable overrides are supported with `KONG_` prefix (e.g., `KONG_PG_PORT=5433`).
Tests also support official Kong-style `KONG_TEST_*` and `KONG_SPEC_TEST_*` variables. The test runner maps them to effective `KONG_*` values before invoking `cargo test`, and defaults to `KONG_DATABASE=postgres` to match Kong's default test strategy.

Examples:

```bash
KONG_TEST_DATABASE=postgres KONG_TEST_PG_PORT=55432 make test
KONG_TEST_DATABASE=off make test
./scripts/run-cargo-test.sh --print-effective-env
```

## Development

| Command | Description |
|---------|-------------|
| `make build` | Build (debug) |
| `make check` | Fast type check |
| `make test` | Run all tests (defaults to `KONG_TEST_DATABASE=postgres`) |
| `make test-pg` | Start local PostgreSQL test dependency and run tests with `KONG_TEST_DATABASE=postgres` |
| `make test-dbless` | Run tests with `KONG_TEST_DATABASE=off` |
| `make fmt` | Format code |
| `make lint` | Clippy analysis |
| `make dev` | Full-stack start (PG + bootstrap + run) |
| `make dev-dbless` | DB-less mode start |

See the [Makefile](Makefile) for all available commands.

## Compatibility

Kong-Rust aims for 100% behavioral compatibility with Kong Gateway:

- **Admin API** — All CRUD endpoints for Services, Routes, Consumers, Plugins, Upstreams, Targets, Certificates, SNIs, CA Certificates, and Vaults
- **Database Schema** — Directly operates on Kong's PostgreSQL tables (no ORM, raw SQL via sqlx)
- **Configuration** — Reads `kong.conf` in the same key=value format
- **Lua Plugins** — Runs Kong Lua plugins through mlua + LuaJIT with full PDK support
- **Migration** — Use `decK dump` from existing Kong, then import into Kong-Rust

## Project Status

### Traditional Gateway

| Phase | Status | Description |
|-------|--------|-------------|
| 1. Core Models | Done | Data models, traits, configuration |
| 2. Database | Done | PostgreSQL DAO, caching, db-less, migrations |
| 3. Router | Done | Traditional + expressions routing |
| 4. Proxy Engine | Done | Pingora integration, load balancing, health checks |
| 5. Plugin System | Done | Plugin registry, Lua bridge, PDK |
| 6. Admin API | Done | Full CRUD, nested endpoints, Kong Manager support |
| 7. TLS | Done | Certificate management, SNI routing |
| 8. Integration | Done | End-to-end testing, access logs, L4 stream proxy |
| 9. Hybrid Mode | Planned | CP/DP cluster communication |

### AI Gateway Roadmap (Dual-Track Parallel)

| Phase | Track | Status | Description |
|-------|-------|--------|-------------|
| Phase 0 | A | In Progress | Stability hardening — Kong official spec test alignment |
| Phase 2a-MVP | B | Planned | LLM Gateway MVP — OpenAI protocol proxy, token counting |
| Phase 1 | A | Planned | Hybrid CP/DP mode (traditional gateway completion) |
| Phase 2a-Full | B | Planned | Multi-model LB & fallback (Anthropic, Gemini) |
| Phase 2b | B | Planned | Virtual API keys, token cost tracking |
| Phase 2c | B | Planned | Semantic caching |
| Phase 2d | B | Planned | Prompt guard |
| Phase 3 | B | Planned | MCP Gateway — Server registration, discovery, routing |
| Phase 4 | B | Planned | Agent Gateway — A2A protocol, agent routing, identity management |
| Phase 5a | C | Planned | AI Gateway Console — Replace Kong Manager OSS with modern UI |
| Phase 5b | C | Planned | LLM management panel — Provider config, cost dashboard, call logs |
| Phase 5c | C | Planned | Agent/MCP panel — Agent topology, tool tracing, skill canvas |

**All AI capabilities will be implemented in Rust-native code** — no Lua plugins. This is Kong-Rust's core performance advantage over Kong (Lua) and LiteLLM (Python).

See [docs/designs/kong-rust-roadmap.md](docs/designs/kong-rust-roadmap.md) for the full strategic roadmap.

## Documentation

| Document | Description |
|----------|-------------|
| [AI Gateway Strategy](docs/designs/ai-gateway-strategy.md) | AI gateway positioning & dual-track execution plan |
| [Roadmap](docs/designs/kong-rust-roadmap.md) | Hybrid mode detailed design & legacy roadmap |
| [Design](docs/design.md) | Architecture & component design |
| [Requirements](docs/requirements.md) | Functional & non-functional requirements |
| [Tasks](docs/tasks.md) | Task tracking & progress |
| [TODOs](TODOS.md) | Prioritized backlog |

## License

Apache-2.0

## Acknowledgments

- [Kong Gateway](https://github.com/Kong/kong) — The API gateway this project is compatible with
- [Pingora](https://github.com/cloudflare/pingora) — Cloudflare's Rust HTTP proxy framework
- [axum](https://github.com/tokio-rs/axum) — Ergonomic Rust web framework
- [mlua](https://github.com/mlua-rs/mlua) — Rust bindings for Lua/LuaJIT
