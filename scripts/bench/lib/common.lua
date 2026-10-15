local source = debug.getinfo(1, "S").source:sub(2)
local dir = source:match("^(.*)/") or "."
package.path = dir .. "/?.lua;" .. package.path

local json = require("json")
local M = { json = json }

function M.read(path)
  local file, err = io.open(path, "rb")
  if not file then error(err, 2) end
  local value = file:read("*a")
  file:close()
  return value
end

function M.write(path, value)
  local file, err = io.open(path, "wb")
  if not file then error(err, 2) end
  file:write(value)
  file:close()
end

function M.read_json(path)
  return json.decode(M.read(path))
end

function M.read_jsonl(path)
  local rows = {}
  for line in (M.read(path) .. "\n"):gmatch("(.-)\n") do
    if line:match("%S") then rows[#rows + 1] = json.decode(line) end
  end
  return rows
end

local function is_array(value)
  local mt = getmetatable(value)
  if mt == json.array_mt then return true end
  if mt == json.object_mt then return false end
  local count, max = 0, 0
  for key in pairs(value) do
    if type(key) ~= "number" or key < 1 or key % 1 ~= 0 then return false end
    count = count + 1
    if key > max then max = key end
  end
  return count == max
end

local function pretty(value, depth)
  if value == json.null or type(value) ~= "table" then return json.encode(value) end
  local indent = string.rep("  ", depth)
  local child_indent = string.rep("  ", depth + 1)
  local parts = {}
  if is_array(value) then
    for i = 1, #value do parts[#parts + 1] = child_indent .. pretty(value[i], depth + 1) end
    if #parts == 0 then return "[]" end
    return "[\n" .. table.concat(parts, ",\n") .. "\n" .. indent .. "]"
  end
  local keys = {}
  for key in pairs(value) do keys[#keys + 1] = key end
  table.sort(keys)
  for _, key in ipairs(keys) do
    parts[#parts + 1] = child_indent .. json.encode(key) .. ": " .. pretty(value[key], depth + 1)
  end
  if #parts == 0 then return "{}" end
  return "{\n" .. table.concat(parts, ",\n") .. "\n" .. indent .. "}"
end

function M.write_pretty(path, value)
  M.write(path, pretty(value, 0) .. "\n")
end

function M.python_string(value)
  if value == nil then return "None" end
  if value == json.null then return "None" end
  if value == true then return "True" end
  if value == false then return "False" end
  return tostring(value)
end

function M.quote(value)
  return "'" .. tostring(value):gsub("'", "'\\''") .. "'"
end

function M.ensure_dir(path)
  local rc = os.execute("mkdir -p " .. M.quote(path))
  if rc ~= 0 then error("cannot create directory: " .. path, 2) end
end

function M.parse_flags(argv, names, required, usage)
  local values = {}
  local reverse = {}
  for option, name in pairs(names) do reverse[name] = option end
  local script = (arg[0] or "script"):match("([^/]+)$") or "script"
  local function fail(message, code)
    io.stderr:write("usage: " .. usage .. "\n" .. script .. ": error: " .. message .. "\n")
    os.exit(code)
  end
  local i = 1
  while i <= #argv do
    local option = argv[i]
    if option == "--help" then
      io.stdout:write("usage: " .. usage .. "\n")
      os.exit(0)
    end
    local name = names[option]
    if not name then fail("unrecognized arguments: " .. option, 2) end
    if i == #argv or argv[i + 1]:sub(1, 2) == "--" then
      fail("argument " .. option .. ": expected one argument", 2)
    end
    values[name] = argv[i + 1]
    i = i + 2
  end
  for _, name in ipairs(required) do
    if values[name] == nil then fail("the following arguments are required: " .. reverse[name], 2) end
  end
  return values
end

return M
