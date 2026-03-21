-- spec/00-smoke/02-mock_upstream_spec.lua
-- Smoke tests for mock upstream server — Mock upstream 服务器烟雾测试

local helpers = require("spec.helpers")
local http_client = require("spec.fixtures.http_client")
local cjson = require("cjson")
local socket = require("socket")

-- Start mock upstream before all tests — 在所有测试前启动 mock upstream
setup(function()
    helpers.start_mock_upstream()
end)

-- Stop mock upstream after all tests — 在所有测试后停止 mock upstream
teardown(function()
    helpers.stop_mock_upstream()
end)

local function mock_client()
    return http_client.new("127.0.0.1", helpers.mock_upstream_port, { timeout = 5 })
end

describe("mock upstream", function()

    describe("GET /", function()
        it("returns valid routes listing", function()
            local client = mock_client()
            local res = client:get("/")
            assert.are.equal(200, res.status)
            assert.are.equal("mock_upstream", res.headers["x-powered-by"])
            assert.are.equal("mock-upstream/1.0.0", res.headers["server"])

            local body = cjson.decode(res.body)
            assert.is_not_nil(body.valid_routes)
            assert.is_not_nil(body.valid_routes["/get"])
            assert.is_not_nil(body.headers)
            assert.is_not_nil(body.vars)
        end)

        it("rejects non-GET methods", function()
            local client = mock_client()
            local res = client:post("/", { body = "", headers = {} })
            assert.are.equal(405, res.status)
        end)
    end)

    describe("GET /get", function()
        it("echoes GET request details", function()
            local client = mock_client()
            local res = client:get("/get")
            assert.are.equal(200, res.status)

            local body = cjson.decode(res.body)
            assert.is_not_nil(body.headers)
            assert.is_not_nil(body.vars)
            assert.are.equal("GET", body.vars.request_method)
            assert.are.equal("/get", body.vars.uri)
        end)

        it("rejects POST", function()
            local client = mock_client()
            local res = client:post("/get", { body = "", headers = {} })
            assert.are.equal(405, res.status)
        end)
    end)

    describe("POST /post", function()
        it("echoes POST request with JSON body", function()
            local client = mock_client()
            local res = client:post("/post", {
                body = cjson.encode({ key = "value" }),
                headers = { ["Content-Type"] = "application/json" },
            })
            assert.are.equal(200, res.status)

            local body = cjson.decode(res.body)
            assert.are.equal("POST", body.vars.request_method)
            assert.is_not_nil(body.post_data)
            assert.are.equal("json", body.post_data.kind)
            assert.are.equal("value", body.post_data.params.key)
        end)

        it("echoes POST with form data", function()
            local client = mock_client()
            local res = client:post("/post", {
                body = "foo=bar&baz=qux",
                headers = { ["Content-Type"] = "application/x-www-form-urlencoded" },
            })
            assert.are.equal(200, res.status)

            local body = cjson.decode(res.body)
            assert.are.equal("form", body.post_data.kind)
            assert.are.equal("bar", body.post_data.params.foo)
            assert.are.equal("qux", body.post_data.params.baz)
        end)
    end)

    describe("ANY /anything", function()
        it("echoes any method", function()
            local client = mock_client()
            local res = client:get("/anything")
            assert.are.equal(200, res.status)

            local body = cjson.decode(res.body)
            assert.are.equal("GET", body.vars.request_method)
        end)
    end)

    describe("ANY /request", function()
        it("is an alias for /anything", function()
            local client = mock_client()
            local res = client:get("/request")
            assert.are.equal(200, res.status)

            local body = cjson.decode(res.body)
            assert.are.equal("GET", body.vars.request_method)
        end)
    end)

    describe("GET /xml", function()
        it("returns XML document", function()
            local client = mock_client()
            local res = client:get("/xml")
            assert.are.equal(200, res.status)
            assert.truthy(res.headers["content-type"]:find("application/xml"))
            assert.truthy(res.body:find("Kong, Monolith destroyer"))
        end)
    end)

    describe("ANY /status/{code}", function()
        it("returns specified status code", function()
            local client = mock_client()
            local res = client:get("/status/201")
            assert.are.equal(201, res.status)

            local body = cjson.decode(res.body)
            assert.are.equal(201, body.code)
        end)

        it("returns 404 status", function()
            local client = mock_client()
            local res = client:get("/status/404")
            assert.are.equal(404, res.status)
        end)

        it("returns 503 status", function()
            local client = mock_client()
            local res = client:get("/status/503")
            assert.are.equal(503, res.status)
        end)
    end)

    describe("ANY /delay/{seconds}", function()
        it("delays response", function()
            local client = mock_client()
            local start = socket.gettime()
            local res = client:get("/delay/1")
            local elapsed = socket.gettime() - start
            assert.are.equal(200, res.status)

            local body = cjson.decode(res.body)
            assert.are.equal(1, body.delay)
            -- Should take at least 0.8 seconds (allowing some slack)
            -- 应该至少用 0.8 秒（允许一些误差）
            assert.truthy(elapsed >= 0.8)
        end)
    end)

    describe("GET /response-headers", function()
        it("sets custom response headers from query params", function()
            local client = mock_client()
            local res = client:get("/response-headers?X-Custom-Header=hello&X-Another=world")
            assert.are.equal(200, res.status)
            assert.are.equal("hello", res.headers["x-custom-header"])
            assert.are.equal("world", res.headers["x-another"])
        end)
    end)

    describe("GET /cache/{n}", function()
        it("sets Cache-Control header", function()
            local client = mock_client()
            local res = client:get("/cache/3600")
            assert.are.equal(200, res.status)
            assert.are.equal("public, max-age=3600", res.headers["cache-control"])
        end)
    end)

    describe("ANY /basic-auth/{user}/{pass}", function()
        it("returns 401 without credentials", function()
            local client = mock_client()
            local res = client:get("/basic-auth/testuser/testpass")
            assert.are.equal(401, res.status)
            assert.are.equal("mock_upstream", res.headers["www-authenticate"])
        end)

        it("authenticates with valid credentials", function()
            local client = mock_client()
            -- base64("testuser:testpass") = "dGVzdHVzZXI6dGVzdHBhc3M="
            local res = client:get("/basic-auth/testuser/testpass", {
                headers = { ["Authorization"] = "Basic dGVzdHVzZXI6dGVzdHBhc3M=" },
            })
            assert.are.equal(200, res.status)

            local body = cjson.decode(res.body)
            assert.is_true(body.authenticated)
            assert.are.equal("testuser", body.user)
        end)

        it("rejects invalid credentials", function()
            local client = mock_client()
            -- base64("wrong:creds") = "d3Jvbmc6Y3JlZHM="
            local res = client:get("/basic-auth/testuser/testpass", {
                headers = { ["Authorization"] = "Basic d3Jvbmc6Y3JlZHM=" },
            })
            assert.are.equal(401, res.status)
        end)
    end)

    describe("ANY /stream/{n}", function()
        it("returns multiple JSON chunks", function()
            local client = mock_client()
            local res = client:get("/stream/3")
            assert.are.equal(200, res.status)

            -- Each line should be a valid JSON object — 每一行应该是有效的 JSON 对象
            local count = 0
            for line in res.body:gmatch("[^\n]+") do
                local ok, json = pcall(cjson.decode, line)
                assert.is_true(ok, "Each line must be valid JSON")
                assert.is_not_nil(json.vars)
                count = count + 1
            end
            assert.are.equal(3, count)
        end)
    end)

    describe("ANY /timestamp", function()
        it("returns Server-Time header", function()
            local client = mock_client()
            local res = client:get("/timestamp")
            assert.are.equal(200, res.status)
            assert.is_not_nil(res.headers["server-time"])
            local ts = tonumber(res.headers["server-time"])
            assert.is_not_nil(ts)
            -- Should be a reasonable Unix timestamp (after 2020) — 应该是一个合理的 Unix 时间戳（2020 年之后）
            assert.truthy(ts > 1577836800)
        end)
    end)

    describe("ANY /hop-by-hop", function()
        it("returns hop-by-hop headers", function()
            local client = mock_client()
            local res = client:get("/hop-by-hop")
            assert.are.equal(200, res.status)
            assert.is_not_nil(res.headers["keep-alive"])
            assert.is_not_nil(res.headers["trailer"])
        end)
    end)

    describe("log endpoints", function()
        it("POST /post_log, GET /read_log, GET /count_log, DELETE /reset_log", function()
            local client = mock_client()

            -- Reset first — 先重置
            client:delete("/reset_log/test_log")

            -- Post a log entry — 发送日志条目
            local res = client:post("/post_log/test_log", {
                body = cjson.encode({ message = "hello", level = "info" }),
                headers = { ["Content-Type"] = "application/json" },
            })
            assert.are.equal(200, res.status)

            -- Post another entry (array format) — 发送另一个条目（数组格式）
            res = client:post("/post_log/test_log", {
                body = cjson.encode({{ message = "world" }, { message = "!" }}),
                headers = { ["Content-Type"] = "application/json" },
            })
            assert.are.equal(200, res.status)

            -- Count should be 3 — 计数应为 3
            res = client:get("/count_log/test_log")
            assert.are.equal(200, res.status)
            assert.are.equal("3", res.body)

            -- Read entries — 读取条目
            res = client:get("/read_log/test_log")
            assert.are.equal(200, res.status)
            local body = cjson.decode(res.body)
            assert.are.equal(3, body.count)
            assert.are.equal(3, #body.entries)
            assert.are.equal("hello", body.entries[1].entry.message)

            -- Reset — 重置
            res = client:delete("/reset_log/test_log")
            assert.are.equal(200, res.status)

            -- Count should be 0 — 计数应为 0
            res = client:get("/count_log/test_log")
            assert.are.equal(200, res.status)
            assert.are.equal("0", res.body)
        end)
    end)

    describe("query parameters", function()
        it("parses uri_args correctly", function()
            local client = mock_client()
            local res = client:get("/anything?foo=bar&baz=qux")
            assert.are.equal(200, res.status)

            local body = cjson.decode(res.body)
            assert.are.equal("bar", body.uri_args.foo)
            assert.are.equal("qux", body.uri_args.baz)
        end)
    end)

    describe("custom headers", function()
        it("echoes request headers", function()
            local client = mock_client()
            local res = client:get("/anything", {
                headers = { ["X-Custom-Test"] = "test-value" },
            })
            assert.are.equal(200, res.status)

            local body = cjson.decode(res.body)
            assert.are.equal("test-value", body.headers["x-custom-test"])
        end)
    end)

end)
