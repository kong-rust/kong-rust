-- kong/tools/uuid.lua
-- Shim module for Kong UUID utilities — Kong UUID 工具 shim 模块

local _M = {}

-- UUID v4 generation using math.random — 使用 math.random 生成 UUID v4
function _M.uuid()
    local template = "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx"
    return string.gsub(template, "[xy]", function(c)
        local v = (c == "x") and math.random(0, 0xf) or math.random(8, 0xb)
        return string.format("%x", v)
    end)
end

-- UUID validation — UUID 格式验证
local UUID_PATTERN = "^%x%x%x%x%x%x%x%x%-%x%x%x%x%-%x%x%x%x%-%x%x%x%x%-%x%x%x%x%x%x%x%x%x%x%x%x$"

function _M.is_valid_uuid(str)
    if type(str) ~= "string" then
        return false
    end
    return str:match(UUID_PATTERN) ~= nil
end

return _M
