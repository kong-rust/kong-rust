-- spec/fixtures/admin_api.lua
-- Shim: Admin API as DB interface — Admin API 作为 DB 接口的 shim
-- Kong spec 使用此模块将 Admin API 当做 DB 直接操作

local helpers = require "spec.helpers"
local cjson = require "cjson"

local function api_send(method, path, body, forced_port)
    local api_client = helpers.admin_client(nil, forced_port)
    local res, err = api_client:send({
        method = method,
        path = path,
        headers = {
            ["Content-Type"] = "application/json"
        },
        body = body,
    })
    if not res then
        api_client:close()
        return nil, err
    end

    if res.status == 204 then
        api_client:close()
        return nil
    end

    local resbody = res:read_body()
    api_client:close()
    if res.status < 300 then
        return cjson.decode(resbody)
    end

    return nil, "Error " .. tostring(res.status) .. ": " .. resbody
end

-- Create a simple entity proxy table — 创建简单的实体代理表
local function make_entity(admin_api_name)
    return {
        insert = function(_, tbl)
            return api_send("POST", "/" .. admin_api_name, tbl)
        end,
        remove = function(_, tbl)
            return api_send("DELETE", "/" .. admin_api_name .. "/" .. tbl.id)
        end,
        update = function(_, id, tbl)
            return api_send("PATCH", "/" .. admin_api_name .. "/" .. id, tbl)
        end,
        truncate = function()
            repeat
                local res = api_send("GET", "/" .. admin_api_name)
                if not res or type(res) ~= "table" or not res.data then return true end
                for _, entity in ipairs(res.data) do
                    api_send("DELETE", "/" .. admin_api_name .. "/" .. entity.id)
                end
            until #res.data == 0
            return true
        end,
    }
end

-- Return a blueprint-like object — 返回类似 blueprint 的对象
local bp = helpers.get_db_utils("postgres", {})
return bp
