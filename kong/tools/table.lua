-- kong/tools/table.lua
-- Shim module for Kong table utilities — Kong 表工具 shim 模块

local _M = {}

-- nkeys: count number of keys in table — 计算表中的键数
function _M.nkeys(t)
    local count = 0
    for _ in pairs(t) do
        count = count + 1
    end
    return count
end

-- table_merge: merge tables — 合并表
function _M.table_merge(t1, t2)
    if not t1 then t1 = {} end
    if not t2 then return t1 end
    local result = {}
    for k, v in pairs(t1) do result[k] = v end
    for k, v in pairs(t2) do result[k] = v end
    return result
end

-- cycle_aware_deep_copy: deep copy with cycle detection — 带环检测的深复制
function _M.cycle_aware_deep_copy(t, seen)
    if type(t) ~= "table" then return t end
    seen = seen or {}
    if seen[t] then return seen[t] end
    local copy = {}
    seen[t] = copy
    for k, v in pairs(t) do
        copy[_M.cycle_aware_deep_copy(k, seen)] = _M.cycle_aware_deep_copy(v, seen)
    end
    return setmetatable(copy, getmetatable(t))
end

return _M
