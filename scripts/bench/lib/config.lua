#!/usr/bin/env luajit
local source = debug.getinfo(1, "S").source:sub(2)
local dir = source:match("^(.*)/") or "."
package.path = dir .. "/?.lua;" .. package.path
local common = require("common")
local json = common.json

local command = table.remove(arg, 1)
local args = arg

local function fail(message, code)
  io.stderr:write(message .. "\n")
  os.exit(code or 1)
end

local function need(count)
  if #args < count then fail("usage: config.lua " .. tostring(command) .. " (missing arguments)", 2) end
end

local function models(path)
  local value = common.read_json(path)
  if type(value) ~= "table" then fail("models config must be a JSON object") end
  return value
end

local function model(cfg, id)
  for _, value in ipairs(cfg.models or {}) do
    if value.id == id then return value end
  end
  return nil
end

local function pytruthy(value)
  if value == nil or value == json.null or value == false then return false end
  if type(value) == "number" then return value ~= 0 end
  if type(value) == "string" then return value ~= "" end
  if type(value) == "table" then return next(value) ~= nil end
  return true
end

local function split_words(value)
  local out = {}
  for word in tostring(value or ""):gmatch("%S+") do out[#out + 1] = word end
  return out
end

local function append_jsonl(path, value)
  local file, err = io.open(path, "a")
  if not file then error(err) end
  file:write(json.encode(value), "\n")
  file:close()
end

local function cell_exists(path, harness, model_id, instance)
  local file = io.open(path, "rb")
  if not file then return false end
  for line in file:lines() do
    if line:match("%S") then
      local cell = json.decode(line)
      if cell.harness == harness and cell.model == model_id and cell.instance_id == instance then
        file:close()
        return true
      end
    end
  end
  file:close()
  return false
end

local function find_files(root, name_pattern, maxdepth)
  local depth = maxdepth and " -maxdepth " .. maxdepth or ""
  local command_line = "find " .. common.quote(root) .. depth .. " -type f -name " .. common.quote(name_pattern) .. " -print 2>/dev/null"
  local pipe = io.popen(command_line, "r")
  if not pipe then return {} end
  local paths = {}
  for path in pipe:lines() do paths[#paths + 1] = path end
  pipe:close()
  table.sort(paths)
  return paths
end

-- Run provenance (from `config.lua provenance`) travels in BENCH_PROVENANCE.
local function add_provenance(meta)
  local raw = os.getenv("BENCH_PROVENANCE")
  if raw and raw ~= "" then meta.provenance = json.decode(raw) end
  meta.finished_utc = os.date("!%Y-%m-%dT%H:%M:%SZ")
end

if command == "json-valid" then
  need(1)
  common.read_json(args[1])
elseif command == "models-check" then
  need(1)
  local cfg = models(args[1])
  assert(type(cfg.models) == "table" and #cfg.models >= 2)
  local ids = {}
  for _, value in ipairs(cfg.models) do ids[#ids + 1] = value.id end
  local id_set = {}
  for _, id in ipairs(ids) do id_set[id] = true end
  assert(id_set["gpt-5.6-sol"])
  assert(id_set["deepseek-v4-flash"])
  assert(id_set["glm-5.3-flash"])
  for _, value in ipairs(cfg.models) do
    assert(pytruthy(value.effort))
    if value.id == "gpt-5.6-sol" or value.id == "gpt-5.6-terra" then
      assert(value.codex and value.pi and value.tk and value.omp and value.fx)
    end
    if value.id == "deepseek-v4-flash" then
      assert(value.opencode and pytruthy(value.opencode.model))
    end
    if value.id == "glm-5.3-flash" then
      assert(value.tk and value.tk.model == "glm-5.3-flash")
      assert(value.tk.provider == "zai")
    end
  end
  local harnesses = {}
  for _, value in ipairs(cfg.harnesses or {}) do harnesses[value] = true end
  assert(harnesses.omp and harnesses.fx and harnesses.opencode)
  table.sort(ids)
  local quoted = {}
  for _, id in ipairs(ids) do quoted[#quoted + 1] = "'" .. id .. "'" end
  print("ok models.json [" .. table.concat(quoted, ", ") .. "]")
elseif command == "default" then
  need(2)
  local cfg = models(args[1])
  local value = cfg[args[2]]
  if value == nil then fail("missing config key: " .. args[2]) end
  print(common.python_string(value))
elseif command == "harnesses" then
  need(1)
  print(table.concat(models(args[1]).harnesses or {}, " "))
elseif command == "model-ids" then
  need(1)
  local cfg = models(args[1])
  local want = {}
  for i = 2, #args do want[#want + 1] = args[i] end
  local ids, id_set = {}, {}
  for _, value in ipairs(cfg.models or {}) do
    ids[#ids + 1] = value.id
    id_set[value.id] = true
  end
  if #want > 0 then
    local missing = {}
    for _, id in ipairs(want) do if not id_set[id] then missing[#missing + 1] = id end end
    if #missing > 0 then fail("unknown model(s): " .. table.concat(missing, ", ")) end
    ids = want
  end
  io.write(table.concat(ids, " "), "\n")
elseif command == "matrix" then
  -- A model runs on a harness only when models.json gives that harness its
  -- own model id; adapter defaults would silently run a different model.
  need(3)
  local cfg = models(args[1])
  local pairs_, excluded = {}, {}
  for _, id in ipairs(split_words(args[2])) do
    local value = model(cfg, id)
    if not value then fail("unknown model: " .. id) end
    for _, harness in ipairs(split_words(args[3])) do
      local entry = value[harness]
      if type(entry) == "table" and type(entry.model) == "string" and entry.model ~= "" then
        pairs_[#pairs_ + 1] = { harness = harness, model = id }
        print("run " .. harness .. " " .. id)
      else
        excluded[#excluded + 1] = { harness = harness, model = id, reason = "not_configured" }
        print("skip " .. harness .. " " .. id .. " not_configured")
      end
    end
  end
  if args[4] and args[5] then
    common.write(args[5], json.encode({
      pairs = setmetatable(pairs_, json.array_mt),
      excluded = setmetatable(excluded, json.array_mt),
      instance_ids_file = args[4],
    }) .. "\n")
  end
elseif command == "provenance" then
  need(3)
  local function capture(cmd)
    local pipe = io.popen(cmd .. " 2>/dev/null", "r")
    if not pipe then return nil end
    local out = pipe:read("*a") or ""
    pipe:close()
    out = out:match("^%s*(.-)%s*$")
    return out ~= "" and out or nil
  end
  local function sha256(path)
    local out = capture("shasum -a 256 " .. common.quote(path))
    return out and out:match("^(%x+)") or json.null
  end
  local repo = common.quote(args[1])
  local status = capture("git -C " .. repo .. " status --porcelain --untracked-files=no")
  io.write(json.encode({
    git_commit = capture("git -C " .. repo .. " rev-parse HEAD") or json.null,
    git_dirty = status ~= nil,
    models_sha256 = sha256(args[2]),
    instance_ids_sha256 = sha256(args[3]),
    started_utc = os.date("!%Y-%m-%dT%H:%M:%SZ"),
    agent_timeout = os.getenv("BENCH_AGENT_TIMEOUT") or json.null,
    tk_max_turns = os.getenv("TK_MAX_TURNS") or json.null,
  }), "\n")
elseif command == "model-get" then
  need(3)
  local value = model(models(args[1]), args[2])
  if not value then fail("unknown model: " .. args[2]) end
  local result = value
  for key in args[3]:gmatch("[^/]+") do
    if type(result) ~= "table" or result[key] == nil then result = nil; break end
    result = result[key]
  end
  if result == nil or result == json.null then
    if args[4] ~= nil then print(args[4]); os.exit(0) end
    fail("missing model field: " .. args[3])
  end
  print(common.python_string(result))
elseif command == "sample-ids" then
  need(2)
  local rows = common.read_jsonl(args[1])
  local ids = {}
  for _, row in ipairs(rows) do
    if row.instance_id == nil then fail("sample row is missing instance_id") end
    ids[#ids + 1] = tostring(row.instance_id)
  end
  common.write(args[2], table.concat(ids, "\n") .. "\n")
  io.stderr:write("sampled " .. #ids .. " instances\n")
elseif command == "instance-field" then
  need(3)
  for _, row in ipairs(common.read_jsonl(args[1])) do
    if row.instance_id == args[2] then
      if row[args[3]] == nil then fail("missing instance field: " .. args[3]) end
      print(common.python_string(row[args[3]]))
      break
    end
  end
elseif command == "cell-exists" then
  need(4)
  if cell_exists(args[1], args[2], args[3], args[4]) then os.exit(0) end
  os.exit(1)
elseif command == "resume-patches" then
  need(4)
  local cells_path, out, harness, model_id = args[1], args[2], args[3], args[4]
  local seen = {}
  local file = io.open(cells_path, "rb")
  if file then
    for line in file:lines() do
      if line:match("%S") then
        local cell = json.decode(line)
        if cell.harness == harness and cell.model == model_id then seen[cell.instance_id] = true end
      end
    end
    file:close()
  end
  local patches = find_files(out .. "/patches", harness .. "__" .. model_id .. "__*.patch", 1)
  for _, patch in ipairs(patches) do
    local instance = patch:match("([^/]+)%.patch$"):sub(#(harness .. "__" .. model_id .. "__") + 1)
    if not seen[instance] then
      append_jsonl(cells_path, {
        harness = harness,
        model = model_id,
        instance_id = instance,
        resolved = json.null,
        seconds = 0,
        adapter_exit = 0,
        patch = patch,
        driver = "thin",
        resumed_from_patch = true,
      })
      seen[instance] = true
      io.stderr:write("resume: recorded existing patch " .. patch:match("([^/]+)$") .. "\n")
    end
  end
elseif command == "predictions" then
  need(4)
  local cells_path, out, harness, model_id = args[1], args[2], args[3], args[4]
  local slug = harness .. "__" .. model_id
  local file, err = io.open(out, "wb")
  if not file then error(err) end
  for _, cell in ipairs(common.read_jsonl(cells_path)) do
    if cell.harness == harness and cell.model == model_id then
      local patch = ""
      if pytruthy(cell.patch) then
        local patch_file = io.open(cell.patch, "rb")
        if patch_file then patch = patch_file:read("*a"); patch_file:close() end
      end
      file:write(json.encode({
        instance_id = cell.instance_id,
        model_name_or_path = slug,
        model_patch = patch,
      }), "\n")
    end
  end
  file:close()
elseif command == "apply-eval" then
  need(4)
  local cells_path, harness, model_id, run_id = args[1], args[2], args[3], args[4]
  local resolved = {}
  local paths = find_files("logs/run_evaluation/" .. run_id, "*.json")
  for _, path in ipairs(paths) do
    local ok, data = pcall(common.read_json, path)
    if ok and type(data) == "table" then
      for key, value in pairs(data) do
        if type(value) == "table" and value.resolved ~= nil then resolved[key] = pytruthy(value.resolved) end
      end
      for _, key in ipairs({ "resolved_ids", "resolved_instances" }) do
        if type(data[key]) == "table" then
          for _, instance in ipairs(data[key]) do resolved[instance] = true end
        end
      end
    end
  end
  local rows = common.read_jsonl(cells_path)
  for _, row in ipairs(rows) do
    if row.harness == harness and row.model == model_id then
      -- Record the attempt even without a verdict so the report can tell
      -- "eval ran, no result" apart from "never evaluated".
      row.eval_attempted = run_id
      if resolved[row.instance_id] ~= nil then
        row.resolved = resolved[row.instance_id]
        row.eval_run_id = run_id
      end
    end
  end
  local file, err = io.open(cells_path, "wb")
  if not file then error(err) end
  for _, row in ipairs(rows) do file:write(json.encode(row), "\n") end
  file:close()
elseif command == "meta-json" then
  need(6)
  local function number(value)
    local n = tonumber(value)
    if not n or n % 1 ~= 0 then fail("invalid integer: " .. value) end
    return n
  end
  local meta = {
    dataset = "princeton-nlp/SWE-bench_Verified",
    n = number(args[1]),
    sample_seed = number(args[2]),
    driver = args[3],
    docker = args[4],
    models = split_words(args[5]),
    harnesses = split_words(args[6]),
  }
  add_provenance(meta)
  io.write(json.encode(meta), "\n")
elseif command == "deep-meta-json" then
  need(7)
  local function number(value)
    local n = tonumber(value)
    if not n or n % 1 ~= 0 then fail("invalid integer: " .. value) end
    return n
  end
  local meta = {
    dataset = "datacurve/deep-swe",
    n = number(args[1]),
    sample_seed = number(args[2]),
    docker = args[3],
    model = args[4],
    effort = args[5],
    harnesses = split_words(args[6]),
    driver = "thin+pier-verifier",
    tasks = args[7],
  }
  add_provenance(meta)
  io.write(json.encode(meta), "\n")
elseif command == "stdin-field" then
  need(2)
  local raw = io.stdin:read("*a")
  local ok, data = pcall(json.decode, raw or "")
  if not ok or type(data) ~= "table" then data = {} end
  local value = data[args[1]]
  if not pytruthy(value) then value = args[2] end
  print(common.python_string(value))
else
  fail("usage: config.lua {json-valid|models-check|default|harnesses|model-ids|matrix|provenance|model-get|sample-ids|instance-field|cell-exists|resume-patches|predictions|apply-eval|meta-json|deep-meta-json|stdin-field} ...", 2)
end
