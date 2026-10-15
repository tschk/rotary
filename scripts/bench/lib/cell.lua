#!/usr/bin/env luajit
local source = debug.getinfo(1, "S").source:sub(2)
local dir = source:match("^(.*)/") or "."
package.path = dir .. "/?.lua;" .. package.path
local common = require("common")

local args = common.parse_flags(arg, {
  ["--cells"] = "cells",
  ["--harness"] = "harness",
  ["--model"] = "model",
  ["--instance"] = "instance",
  ["--seconds"] = "seconds",
  ["--exit"] = "adapter_exit",
  ["--error"] = "error",
  ["--patch"] = "patch",
  ["--driver"] = "driver",
  ["--resolved"] = "resolved",
  ["--reward-json"] = "reward_json",
  ["--verifier"] = "verifier",
  ["--dataset"] = "dataset",
}, { "cells", "harness", "model", "instance", "seconds" },
  "cell.lua --cells CELLS --harness NAME --model ID --instance ID --seconds N [--exit N] [--error TEXT] [--patch PATH] [--driver NAME] [--resolved true|false|null] [--reward-json PATH] [--verifier NAME] [--dataset NAME]")

local function integer(name, default)
  local value = args[name]
  if value == nil then return default end
  local number = tonumber(value)
  if not number or number % 1 ~= 0 then
    io.stderr:write("cell.lua: error: argument --" .. name:gsub("_", "-") .. ": invalid int value: '" .. value .. "'\n")
    os.exit(2)
  end
  return number
end

local resolved = common.json.null
if args.resolved == "true" then resolved = true end
if args.resolved == "false" then resolved = false end
if args.resolved ~= nil and args.resolved ~= "true" and args.resolved ~= "false" and args.resolved ~= "null" then
  io.stderr:write("cell.lua: error: --resolved must be true, false, or null\n")
  os.exit(2)
end

local cell = {
  harness = args.harness,
  model = args.model,
  instance_id = args.instance,
  resolved = resolved,
  seconds = integer("seconds"),
  adapter_exit = integer("adapter_exit", 0),
  driver = args.driver or "thin",
}
for _, key in ipairs({ "patch", "error", "reward_json", "verifier", "dataset" }) do
  if args[key] ~= nil and args[key] ~= "" then cell[key] = args[key] end
end

local parent = args.cells:match("^(.*)/")
if parent then common.ensure_dir(parent) end
local file, err = io.open(args.cells, "a")
if not file then error(err) end
file:write(common.json.encode(cell), "\n")
file:close()
