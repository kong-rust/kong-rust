-- kong/tools/string.lua
-- Shim module for Kong string utilities — Kong 字符串工具 shim 模块

local _M = {}

-- strip: remove leading/trailing whitespace — 去除前后空白
function _M.strip(str)
    if type(str) ~= "string" then
        return str
    end
    return str:match("^%s*(.-)%s*$")
end

-- split: split string by delimiter — 按分隔符分割字符串
function _M.split(str, delim)
    delim = delim or ","
    local result = {}
    for part in (str .. delim):gmatch("(.-)" .. delim:gsub("([%.%+%-%*%?%[%]%^%$%(%)%%])", "%%%1")) do
        result[#result + 1] = part
    end
    return result
end

-- split_once: split by plain delimiter, max 2 parts — 按纯文本分隔符分割一次
function _M.split_once(str, delim)
    local pos = str:find(delim, 1, true)
    if not pos then
        return str, nil
    end
    return str:sub(1, pos - 1), str:sub(pos + #delim)
end

-- splitn: split by plain delimiter, max n parts — 最多分割 n 次
function _M.splitn(str, delim, n)
    local result = {}
    local count = 0
    local start = 1
    while count < n - 1 do
        local pos = str:find(delim, start, true)
        if not pos then break end
        result[#result + 1] = str:sub(start, pos - 1)
        start = pos + #delim
        count = count + 1
    end
    result[#result + 1] = str:sub(start)
    return result, #result
end

-- isplitn: iterator version of splitn — splitn 的迭代器版本
function _M.isplitn(str, delim, n)
    local parts, count = _M.splitn(str, delim, n or 2^31)
    local i = 0
    return function()
        i = i + 1
        return parts[i]
    end
end

-- validate_utf8: validate UTF-8 encoding — 验证 UTF-8 编码
function _M.validate_utf8(str)
    if type(str) ~= "string" then
        return false, 0
    end
    local i = 1
    local len = #str
    while i <= len do
        local b = str:byte(i)
        if b <= 0x7F then
            i = i + 1
        elseif b >= 0xC2 and b <= 0xDF then
            if i + 1 > len then return false, i end
            local b2 = str:byte(i + 1)
            if b2 < 0x80 or b2 > 0xBF then return false, i end
            i = i + 2
        elseif b >= 0xE0 and b <= 0xEF then
            if i + 2 > len then return false, i end
            local b2, b3 = str:byte(i + 1, i + 2)
            if b2 < 0x80 or b2 > 0xBF or b3 < 0x80 or b3 > 0xBF then return false, i end
            i = i + 3
        elseif b >= 0xF0 and b <= 0xF4 then
            if i + 3 > len then return false, i end
            local b2, b3, b4 = str:byte(i + 1, i + 3)
            if b2 < 0x80 or b2 > 0xBF or b3 < 0x80 or b3 > 0xBF or b4 < 0x80 or b4 > 0xBF then return false, i end
            i = i + 4
        else
            return false, i
        end
    end
    return true
end

-- replace_dashes: replace "-" with "_" — 将 "-" 替换为 "_"
function _M.replace_dashes(str)
    return str:gsub("-", "_")
end

-- replace_dashes_lower: lowercase + replace "-" with "_"
function _M.replace_dashes_lower(str)
    return str:lower():gsub("-", "_")
end

-- bytes_to_str: convert bytes to human-readable string — 字节转可读字符串
function _M.bytes_to_str(bytes, unit, scale)
    scale = scale or 2
    if not bytes then return "" end
    local units = {"", "K", "M", "G", "T"}
    local idx = 1
    while bytes >= 1024 and idx < #units do
        bytes = bytes / 1024
        idx = idx + 1
    end
    return string.format("%." .. scale .. "f %sB", bytes, units[idx])
end

return _M
