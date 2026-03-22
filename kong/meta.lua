-- kong/meta.lua
-- Shim module for Kong version metadata — Kong 版本元数据 shim 模块

local _M = {
    _NAME = "kong",
    _VERSION = "3.10.0",
    _VERSION_TABLE = {
        major = 3,
        minor = 10,
        patch = 0,
    },
    _SERVER_TOKENS = "kong/3.10.0",
}

-- version alias — 版本别名
_M.version = _M._VERSION

-- __tostring for version table — 版本表的字符串化
setmetatable(_M._VERSION_TABLE, {
    __tostring = function(t)
        local version = string.format("%d.%d.%d", t.major, t.minor, t.patch)
        if t.suffix then
            version = version .. "-" .. t.suffix
        end
        return version
    end
})

return _M
