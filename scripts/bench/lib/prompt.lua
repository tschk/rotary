#!/usr/bin/env luajit
local source = debug.getinfo(1, "S").source:sub(2)
local dir = source:match("^(.*)/") or "."
package.path = dir .. "/?.lua;" .. package.path
local common = require("common")

local args = common.parse_flags(arg, {
  ["--sample"] = "sample",
  ["--instance-id"] = "instance_id",
  ["--out"] = "out",
}, { "sample", "instance_id", "out" },
  "prompt.lua --sample SAMPLE --instance-id ID --out PATH")

local record
for _, row in ipairs(common.read_jsonl(args.sample)) do
  if row.instance_id == args.instance_id then
    record = row
    break
  end
end
if not record then
  io.stderr:write("unknown instance: " .. args.instance_id .. "\n")
  os.exit(1)
end

local body = "You are solving SWE-bench Verified instance " .. args.instance_id .. ".\n\n"
  .. "The repository is already checked out at the issue base commit.\n"
  .. "Implement a correct fix for the GitHub issue below.\n"
  .. "Do not expand scope. Leave the repo fixed (dirty tree or committed patch is fine).\n"
  .. "Do not rewrite git history. Do not push.\n"
  .. "Do not ask clarifying questions; pick the conservative correct fix and implement it.\n\n"
  .. "ISSUE:\n" .. record.problem_statement .. "\n"
common.write(args.out, body)
