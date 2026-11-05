#!/usr/bin/env luajit
-- Print the verifier reward for one DeepSWE cell, or `null`.
-- Usage: reward.lua JOB_DIR
-- JOB_DIR is the Pier job created for exactly this harness/model/task cell.
-- Only reward.json files inside it count, and there must be exactly one: a
-- reward from another task or a stale trial is never evidence for this cell.
local source = debug.getinfo(1, "S").source:sub(2)
local dir = source:match("^(.*)/") or "."
package.path = dir .. "/../lib/?.lua;" .. package.path
local common = require("common")
local json = common.json

local job = arg[1]
if not job then
  io.stderr:write("usage: reward.lua JOB_DIR\n")
  os.exit(2)
end
local pipe = io.popen("find " .. common.quote(job) .. " -type f -name reward.json -print 2>/dev/null", "r")
local hits = {}
for path in pipe:lines() do hits[#hits + 1] = path end
pipe:close()
if #hits ~= 1 then
  if #hits > 1 then io.stderr:write("reward.lua: " .. #hits .. " reward.json files under " .. job .. "; refusing to pick one\n") end
  print("null")
  os.exit(0)
end
local ok, data = pcall(common.read_json, hits[1])
if not ok or type(data) ~= "table" or type(data.reward) ~= "number" then
  io.stderr:write("reward.lua: unreadable reward in " .. hits[1] .. "\n")
  print("null")
  os.exit(0)
end
print(json.encode({ reward = data.reward, path = hits[1], f2p = data.f2p, p2p = data.p2p }))
