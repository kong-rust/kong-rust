-- table/nkeys.lua
-- Shim: standalone nkeys function — 独立 nkeys 函数 shim
local function nkeys(t)
    local count = 0
    for _ in pairs(t) do
        count = count + 1
    end
    return count
end
return nkeys
