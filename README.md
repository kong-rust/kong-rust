# Kong-Rust

A high-performance API Gateway written in Rust, fully compatible with [Kong Gateway](https://github.com/Kong/kong). Drop-in replacement — zero migration cost.

## Why Kong-Rust?

Kong is the world's most popular open-source API gateway, but it runs on LuaJIT + OpenResty. Kong-Rust rewrites the core engine in Rust using [Cloudflare Pingora](https://github.com/cloudflare/pingora), while maintaining **100% compatibility** with Kong's configuration, Admin API, database schema, and Lua plugin ecosystem.

| | Kong (Lua/OpenResty) | Kong-Rust |
|---|---|---|
| **Proxy Engine** | OpenResty (Nginx + LuaJIT) | Pingora (Rust, multi-threaded) |
| **Admin API** | Lapis (Lua) | axum (Rust) |
| **Database** | pgmoon (Lua) | sqlx (Rust, async) |
| **Lua Plugins** | Native | mlua (LuaJIT binding) |
| **Memory Safety** | Manual (GC + FFI) | Rust ownership system |

## Features

- **Full Kong Compatibility** — Same data models, Admin API, `kong.conf` format, declarative config (YAML/JSON), and Lua plugin interface (PDK + `ngx.*`)
- **High-Performance Proxy** — Pingora's multi-threaded architecture with shared connection pools
- **Dual Routing Engine** — Both `traditional_compatible` and `expressions` router flavors
- **Lua Plugin Support** — Run all 47 built-in Kong Lua plugins via mlua + LuaJIT
- **Load Balancing & Health Checks** — Round-robin, consistent-hashing, active/passive health checks
- **TLS Termination & SNI** — Certificate management with SNI-based routing
- **L4 Stream Proxy** — TCP/TLS passthrough proxy with SNI-based and source/destination CIDR routing
- **Kong Manager UI** — Works with the official Kong Manager frontend
- **Multiple Data Sources** — PostgreSQL or db-less (declarative config) modes
- **Hybrid Mode** — Control Plane / Data Plane separation (planned)

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
 └── kong-cluster       — CP/DP cluster communication (planned)
```

## Quick Start

### Prerequisites

- Rust 1.84+ (with Cargo)
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

## Development

| Command | Description |
|---------|-------------|
| `make build` | Build (debug) |
| `make check` | Fast type check |
| `make test` | Run all tests |
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

| Phase | Status | Description |
|-------|--------|-------------|
| 1. Core Models | Done | Data models, traits, configuration |
| 2. Database | Done | PostgreSQL DAO, caching, db-less, migrations |
| 3. Router | Done | Traditional + expressions routing |
| 4. Proxy Engine | Done | Pingora integration, load balancing, health checks |
| 5. Plugin System | Done | Plugin registry, Lua bridge, PDK |
| 6. Admin API | Done | Full CRUD, nested endpoints, Kong Manager support |
| 7. TLS | Done | Certificate management, SNI routing |
| 8. Integration | Done | End-to-end testing, access logs |
| 8c. Stream Proxy | Done | L4 TCP/TLS passthrough proxy, SNI/CIDR routing |
| 9. Hybrid Mode | Planned | CP/DP cluster communication |

## License

Apache-2.0

## Acknowledgments

- [Kong Gateway](https://github.com/Kong/kong) — The API gateway this project is compatible with
- [Pingora](https://github.com/cloudflare/pingora) — Cloudflare's Rust HTTP proxy framework
- [axum](https://github.com/tokio-rs/axum) — Ergonomic Rust web framework
- [mlua](https://github.com/mlua-rs/mlua) — Rust bindings for Lua/LuaJIT
