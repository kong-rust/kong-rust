- Task: 6.4 修复 Kong Manager SPA 刷新 404（kong-admin）
- Date: 2026-03-13

- Summary:
  - 修复 Kong Manager 在 `8002` 端口下访问 `/services`、`/routes` 等前端路由时，浏览器刷新返回 404 的问题。
  - 为 GUI router 增加 catch-all SPA fallback，将未知页面路径回退到 `index.html`。
  - 保留 `/__km_base__/*` 静态资源和 `kconfig.js` 的优先匹配，避免静态资源请求被前端路由回退吞掉。

- Files Changed:
  - `crates/kong-admin/src/lib.rs`
  - `.spec-workflow/specs/kong-rust/tasks.md`

- Artifacts:
  - **GUI Router:** `build_gui_router()`
  - **SPA Fallback Route:** `GET /{*path}` -> `index.html`
  - **Static Assets:** `/__km_base__/*` 继续由 `ServeDir` 提供

- Rationale:
  - Kong Manager 是单页应用，客户端路由如 `/services`、`/routes` 在前端导航时可正常工作，但浏览器刷新会把当前路径直接发给服务器。
  - 之前 GUI server 只处理 `/` 和 `/__km_base__/*`，未覆盖 `/services` 这类路径，因此刷新后返回 404。
  - 增加服务端 SPA fallback 后，前端路由可在首次访问和刷新时都正确加载。
