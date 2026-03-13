- Task: 10.6 明确 Docker 端口语义并加固容器默认 Admin API 暴露
- Date: 2026-03-13

- Summary:
  - 修复容器内 Admin API 默认仅绑定 `127.0.0.1:8001`，导致发布 `8001` 端口后容器外不可访问的问题。
  - 明确 `8001` 是 Admin API，`8002` 是 Kong Manager GUI；`/services` 等 CRUD 端点属于 `8001`。
  - 保持 Docker 默认配置文件查找行为与 Kong 一致，不在镜像启动命令中显式指定配置文件路径。

- Files Changed:
  - `docker-entrypoint.sh`
  - `README.md`
  - `README_CN.md`
  - `.spec-workflow/specs/kong-rust/tasks.md`
  - `.spec-workflow/steering/tech.md`

- Artifacts:
  - **Environment Defaults:** 容器默认设置 `KONG_ADMIN_LISTEN=0.0.0.0:8001`
  - **Port Semantics:** `8001` = Admin API, `8002` = Kong Manager GUI

- Rationale:
  - 代码默认 `admin_listen` 是 `127.0.0.1:8001`，在 Docker 端口映射场景下会让宿主机和外部客户端无法访问 Admin API。
  - 用户在服务器上访问 `http://127.0.0.1:8002/services` 返回 `404`，本质上是因为 `8002` 是 GUI 端口，不承载 Admin API 路由。
  - 按用户要求，配置文件查找顺序保持与 Kong 一致，不通过 Docker 默认命令改变查找行为。
