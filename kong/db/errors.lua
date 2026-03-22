-- kong/db/errors.lua
-- Shim module for Kong DB error types — Kong DB 错误类型 shim 模块

local _M = {}

-- Error codes — 错误码
_M.codes = {
    INVALID_PRIMARY_KEY   = 1,
    SCHEMA_VIOLATION      = 2,
    PRIMARY_KEY_VIOLATION  = 3,
    FOREIGN_KEY_VIOLATION  = 4,
    UNIQUE_VIOLATION      = 5,
    NOT_FOUND             = 6,
    INVALID_OFFSET        = 7,
    DATABASE_ERROR        = 8,
    INVALID_SIZE          = 9,
    INVALID_UNIQUE        = 10,
    INVALID_OPTIONS       = 11,
    OPERATION_UNSUPPORTED  = 12,
    FOREIGN_KEYS_UNRESOLVED = 13,
    DECLARATIVE_CONFIG     = 14,
    TRANSFORMATION_ERROR   = 15,
    INVALID_FOREIGN_KEY    = 16,
    INVALID_WORKSPACE      = 17,
    INVALID_UNIQUE_GLOBAL  = 18,
    REFERENCED_BY_OTHERS   = 19,
    INVALID_SEARCH_QUERY   = 20,
}

-- Error names — 错误名称
_M.names = {
    [1]  = "invalid primary key",
    [2]  = "schema violation",
    [3]  = "primary key violation",
    [4]  = "foreign key violation",
    [5]  = "unique constraint violation",
    [6]  = "not found",
    [7]  = "invalid offset",
    [8]  = "database error",
    [9]  = "invalid size",
    [10] = "invalid unique",
    [11] = "invalid options",
    [12] = "operation unsupported",
    [13] = "foreign keys unresolved",
    [14] = "declarative config",
    [15] = "transformation error",
    [16] = "invalid foreign key",
    [17] = "invalid workspace",
    [18] = "invalid unique global",
    [19] = "referenced by others",
    [20] = "invalid search query",
}

-- Error object constructor — 错误对象构造函数
local Error = {}
Error.__index = Error

function Error:__tostring()
    return self.message or self.name or "unknown error"
end

local function new_error(code, message, fields)
    return setmetatable({
        code = code,
        name = _M.names[code] or "unknown",
        message = message,
        fields = fields,
    }, Error)
end

-- Factory constructor — 工厂构造函数
function _M.new(strategy)
    local self = {
        strategy = strategy or "postgres",
    }

    function self:schema_violation(errors)
        return nil, new_error(_M.codes.SCHEMA_VIOLATION,
            "schema violation", errors), _M.codes.SCHEMA_VIOLATION
    end

    function self:unique_violation(unique_key)
        return nil, new_error(_M.codes.UNIQUE_VIOLATION,
            "UNIQUE violation detected on '" .. tostring(unique_key) .. "'"),
            _M.codes.UNIQUE_VIOLATION
    end

    function self:not_found(pk)
        return nil, new_error(_M.codes.NOT_FOUND, "could not find the entity"),
            _M.codes.NOT_FOUND
    end

    function self:foreign_key_violation_invalid_reference(key, parent, pk)
        return nil, new_error(_M.codes.FOREIGN_KEY_VIOLATION,
            string.format("the foreign key '%s' does not reference an existing '%s' entity.",
                tostring(key), tostring(parent))),
            _M.codes.FOREIGN_KEY_VIOLATION
    end

    function self:primary_key_violation(pk)
        return nil, new_error(_M.codes.PRIMARY_KEY_VIOLATION,
            "primary key violation"), _M.codes.PRIMARY_KEY_VIOLATION
    end

    function self:invalid_primary_key(pk)
        return nil, new_error(_M.codes.INVALID_PRIMARY_KEY,
            "invalid primary key"), _M.codes.INVALID_PRIMARY_KEY
    end

    function self:database_error(msg)
        return nil, new_error(_M.codes.DATABASE_ERROR, msg),
            _M.codes.DATABASE_ERROR
    end

    return self
end

return _M
