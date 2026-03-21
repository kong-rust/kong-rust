-- spec/00-smoke/01-admin_api_spec.lua
-- 烟雾测试：验证 busted + spec.helpers + Kong-Rust 端到端工作

local helpers = require "spec.helpers"
local cjson = require "cjson"

describe("Kong-Rust smoke test", function()
    local admin_client

    lazy_setup(function()
        assert(helpers.start_kong({
            database = helpers.test_conf.database,
        }))

        admin_client = helpers.admin_client()
    end)

    lazy_teardown(function()
        if admin_client then
            admin_client:close()
        end
        helpers.stop_kong()
    end)

    describe("Admin API", function()
        it("GET / returns node information", function()
            local res = admin_client:get("/")
            assert.are.equal(200, res.status)
            local body = cjson.decode(res.body)
            assert.is_not_nil(body.version)
            assert.is_not_nil(body.node_id)
        end)

        it("GET /status returns status information", function()
            local res = admin_client:get("/status")
            assert.are.equal(200, res.status)
            local body = cjson.decode(res.body)
            assert.is_not_nil(body.database)
        end)

        it("can CRUD a service", function()
            -- 创建服务
            local res = admin_client:post("/services", {
                body = {
                    name = "smoke-test-service",
                    url = "http://httpbin.org:80",
                },
                headers = { ["Content-Type"] = "application/json" },
            })
            assert.are.equal(201, res.status)
            local service = cjson.decode(res.body)
            assert.are.equal("smoke-test-service", service.name)
            assert.is_not_nil(service.id)

            -- 读取服务
            res = admin_client:get("/services/" .. service.id)
            assert.are.equal(200, res.status)

            -- 更新服务
            res = admin_client:patch("/services/" .. service.id, {
                body = { name = "smoke-test-updated" },
                headers = { ["Content-Type"] = "application/json" },
            })
            assert.are.equal(200, res.status)
            local updated = cjson.decode(res.body)
            assert.are.equal("smoke-test-updated", updated.name)

            -- 删除服务
            res = admin_client:delete("/services/" .. service.id)
            assert.are.equal(204, res.status)
        end)

        it("can create a route with a service", function()
            -- 创建服务
            local res = admin_client:post("/services", {
                body = {
                    name = "route-test-service",
                    url = "http://httpbin.org:80",
                },
                headers = { ["Content-Type"] = "application/json" },
            })
            assert.are.equal(201, res.status)
            local service = cjson.decode(res.body)

            -- 创建路由
            res = admin_client:post("/services/" .. service.id .. "/routes", {
                body = {
                    paths = { "/smoke-test" },
                },
                headers = { ["Content-Type"] = "application/json" },
            })
            assert.are.equal(201, res.status)
            local route = cjson.decode(res.body)
            assert.is_not_nil(route.id)

            -- 清理
            admin_client:delete("/routes/" .. route.id)
            admin_client:delete("/services/" .. service.id)
        end)
    end)

    describe("Blueprint", function()
        it("can create fixtures via Blueprint", function()
            local bp = helpers.get_db_utils("postgres", {
                "services", "routes",
            })

            local service = bp.services:insert({
                name = "bp-test-service",
                url = "http://httpbin.org:80",
            })
            assert.is_not_nil(service.id)

            local route = bp.routes:insert({
                service = { id = service.id },
                paths = { "/bp-test" },
            })
            assert.is_not_nil(route.id)

            -- 清理
            local admin = helpers.admin_client()
            admin:delete("/routes/" .. route.id)
            admin:delete("/services/" .. service.id)
        end)
    end)
end)
