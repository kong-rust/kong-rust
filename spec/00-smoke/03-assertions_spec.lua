-- spec/00-smoke/03-assertions_spec.lua
-- Assertion system alignment tests — 断言系统对齐测试
-- Validates that custom assertions match Kong spec/internal/asserts.lua API

local cjson = require("cjson")

-- Helper: create a mock HTTP response object — 创建模拟 HTTP 响应对象
local function mock_response(status, body, headers)
    headers = headers or {}
    return {
        status = status,
        body = body or "",
        headers = headers,
        read_body = function(self)
            return self.body
        end,
    }
end

-- Helper: create a mock_upstream response (echo response) — 创建 mock_upstream 回显响应
local function mock_upstream_response(request_data)
    local body = cjson.encode(request_data)
    return mock_response(200, body, {
        ["content-type"] = "application/json",
        ["x-powered-by"] = "mock_upstream",
    })
end

describe("Kong custom assertions", function()

    -- Load helpers to register all assertions — 加载 helpers 以注册所有断言
    require("spec.helpers")

    -----------------------------------------------------------------
    -- assert.response(res).has.status() — 响应状态码断言
    -----------------------------------------------------------------
    describe("assert.response().has.status()", function()

        it("passes when status matches", function()
            local res = mock_response(200, '{"ok":true}')
            local body = assert.response(res).has.status(200)
            assert.is_string(body)
        end)

        it("fails when status does not match", function()
            local res = mock_response(404, "not found")
            assert.has_error(function()
                assert.response(res).has.status(200)
            end)
        end)

        it("supports direct form: assert.has.status(200, res)", function()
            local res = mock_response(201, '{"id":"abc"}')
            local body = assert.has.status(201, res)
            assert.is_string(body)
        end)
    end)

    -----------------------------------------------------------------
    -- assert.response(res).has.header() — 响应 Header 断言
    -----------------------------------------------------------------
    describe("assert.response().has.header()", function()

        it("passes when header exists (case-insensitive)", function()
            local res = mock_response(200, "", { ["content-type"] = "application/json" })
            local val = assert.response(res).has.header("Content-Type")
            assert.equal("application/json", val)
        end)

        it("fails when header does not exist", function()
            local res = mock_response(200, "", {})
            assert.has_error(function()
                assert.response(res).has.header("X-Missing")
            end)
        end)

        it("passes with no.header when header is absent", function()
            local res = mock_response(200, "", {})
            assert.response(res).has.no.header("X-Missing")
        end)
    end)

    -----------------------------------------------------------------
    -- assert.response(res).has.jsonbody() — JSON body 断言
    -----------------------------------------------------------------
    describe("assert.response().has.jsonbody()", function()

        it("returns parsed JSON table", function()
            local res = mock_response(200, '{"key":"value"}')
            local json = assert.response(res).has.jsonbody()
            assert.equal("value", json.key)
        end)

        it("fails on invalid JSON", function()
            local res = mock_response(200, "not json at all")
            assert.has_error(function()
                assert.response(res).has.jsonbody()
            end)
        end)
    end)

    -----------------------------------------------------------------
    -- assert.request(res).has.header() — 请求 Header 断言
    -----------------------------------------------------------------
    describe("assert.request().has.header()", function()

        it("extracts header from mock_upstream echo", function()
            local res = mock_upstream_response({
                headers = {
                    ["x-forwarded-for"] = "10.0.0.1",
                    host = "example.com",
                },
                uri = "/request",
                method = "GET",
            })
            local val = assert.request(res).has.header("X-Forwarded-For")
            assert.equal("10.0.0.1", val)
        end)

        it("fails when proxied header is missing", function()
            local res = mock_upstream_response({
                headers = { host = "example.com" },
                uri = "/request",
                method = "GET",
            })
            assert.has_error(function()
                assert.request(res).has.header("X-Missing-Header")
            end)
        end)
    end)

    -----------------------------------------------------------------
    -- assert.request(res).has.queryparam() — Query 参数断言
    -----------------------------------------------------------------
    describe("assert.request().has.queryparam()", function()

        it("finds query parameter from mock_upstream echo", function()
            local res = mock_upstream_response({
                headers = { host = "example.com" },
                uri_args = { foo = "bar", baz = "123" },
                method = "GET",
            })
            local val = assert.request(res).has.queryparam("foo")
            assert.equal("bar", val)
        end)

        it("fails when query parameter is missing", function()
            local res = mock_upstream_response({
                headers = { host = "example.com" },
                uri_args = { foo = "bar" },
                method = "GET",
            })
            assert.has_error(function()
                assert.request(res).has.queryparam("missing")
            end)
        end)
    end)

    -----------------------------------------------------------------
    -- assert.request(res).has.formparam() — 表单参数断言
    -----------------------------------------------------------------
    describe("assert.request().has.formparam()", function()

        it("finds form parameter from mock_upstream echo", function()
            local res = mock_upstream_response({
                headers = { ["content-type"] = "application/x-www-form-urlencoded" },
                post_data = {
                    kind = "form",
                    params = { hello = "world", count = "42" },
                },
                method = "POST",
            })
            local val = assert.request(res).has.formparam("hello")
            assert.equal("world", val)
        end)
    end)

    -----------------------------------------------------------------
    -- assert.contains() — 数组包含断言
    -----------------------------------------------------------------
    describe("assert.contains()", function()

        it("finds exact value in array", function()
            local arr = { "one", "two", "three" }
            local i = assert.contains("two", arr)
            assert.equal(2, i)
        end)

        it("finds pattern match in array", function()
            local arr = { "hello-world", "foo-bar", "baz-qux" }
            local i = assert.contains("^foo", arr, true)
            assert.equal(2, i)
        end)

        it("fails when value not in array", function()
            local arr = { "one", "two" }
            assert.has_error(function()
                assert.contains("three", arr)
            end)
        end)
    end)

    -----------------------------------------------------------------
    -- assert.gt() — 大于断言
    -----------------------------------------------------------------
    describe("assert.gt()", function()

        it("passes when value is greater", function()
            assert.gt(5, 10)  -- 10 > 5
        end)

        it("fails when value is not greater", function()
            assert.has_error(function()
                assert.gt(10, 5)  -- 5 > 10 fails
            end)
        end)
    end)

    -----------------------------------------------------------------
    -- assert.certificate().has.cn() — 证书 CN 断言
    -----------------------------------------------------------------
    describe("assert.certificate().has.cn()", function()

        it("extracts CN from certificate text", function()
            local cert_text = "Subject: C=US, ST=CA, CN=ssl-example.com, O=Kong"
            assert.certificate(cert_text).has.cn("ssl-example.com")
        end)

        it("works with direct form", function()
            local cert_text = "CN = test.example.com"
            assert.cn("test.example.com", cert_text)
        end)

        it("fails when CN does not match", function()
            local cert_text = "CN=wrong.example.com"
            assert.has_error(function()
                assert.cn("expected.example.com", cert_text)
            end)
        end)
    end)

    -----------------------------------------------------------------
    -- assert.partial_match() — 部分表匹配断言
    -----------------------------------------------------------------
    describe("assert.partial_match()", function()

        it("passes when subset matches", function()
            local partial = { name = "test", status = "active" }
            local full = { name = "test", status = "active", id = "123", extra = true }
            assert.partial_match(partial, full)
        end)

        it("passes with nested tables", function()
            local partial = { config = { timeout = 5000 } }
            local full = { config = { timeout = 5000, retries = 3 }, name = "svc" }
            assert.partial_match(partial, full)
        end)

        it("fails when a field doesn't match", function()
            local partial = { name = "test", status = "inactive" }
            local full = { name = "test", status = "active" }
            assert.has_error(function()
                assert.partial_match(partial, full)
            end)
        end)
    end)

    -----------------------------------------------------------------
    -- assert.fail() — 通用失败断言
    -----------------------------------------------------------------
    describe("assert.fail()", function()

        it("always fails", function()
            assert.has_error(function()
                assert.fail("this", "should", "fail")
            end)
        end)
    end)

    -----------------------------------------------------------------
    -- Logfile/line assertion — 日志文件行断言
    -----------------------------------------------------------------
    describe("assert.logfile().has.line()", function()

        it("finds plain string in log file", function()
            -- Create a temporary log file — 创建临时日志文件
            local tmpfile = os.tmpname()
            local f = io.open(tmpfile, "w")
            f:write("2024-01-01 INFO starting server\n")
            f:write("2024-01-01 ERROR something went wrong\n")
            f:write("2024-01-01 INFO server ready\n")
            f:close()

            assert.logfile(tmpfile).has.line("ERROR something went wrong", true, 0)

            os.remove(tmpfile)
        end)

        it("does not find absent string", function()
            local tmpfile = os.tmpname()
            local f = io.open(tmpfile, "w")
            f:write("2024-01-01 INFO all good\n")
            f:close()

            assert.logfile(tmpfile).has.no.line("CRITICAL", true, 0)

            os.remove(tmpfile)
        end)
    end)

    -----------------------------------------------------------------
    -- helpers.unindent — 去缩进辅助函数
    -----------------------------------------------------------------
    describe("helpers.unindent()", function()
        local helpers = require("spec.helpers")

        it("removes common leading whitespace", function()
            local result = helpers.unindent([[
                hello world
                foo bar
            ]])
            assert.equal("hello world\nfoo bar", result)
        end)
    end)

    -----------------------------------------------------------------
    -- helpers.lookup — 大小写不敏感查找
    -----------------------------------------------------------------
    describe("helpers.lookup()", function()
        local helpers = require("spec.helpers")

        it("finds key case-insensitively", function()
            local t = { ["Content-Type"] = "application/json" }
            local val = helpers.lookup(t, "content-type")
            assert.equal("application/json", val)
        end)

        it("returns nil for missing key", function()
            local t = { foo = "bar" }
            local val = helpers.lookup(t, "missing")
            assert.is_nil(val)
        end)
    end)
end)
