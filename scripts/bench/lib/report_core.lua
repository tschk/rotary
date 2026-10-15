local common = require("common")
local M = {}

local function pystr(value)
  return common.python_string(value)
end

local function rate(rows, deep_swe)
  local scored, ok = 0, 0
  for _, row in ipairs(rows) do
    if row.resolved == true or row.resolved == false then
      scored = scored + 1
      if row.resolved == true then ok = ok + 1 end
    end
  end
  if scored == 0 then
    return deep_swe and "n/a (no Pier verifier reward.json)" or "n/a (no official eval)"
  end
  local percent = ok / scored * 100
  local whole = math.floor(percent)
  local remainder = percent - whole
  if remainder > 0.5 or (remainder == 0.5 and whole % 2 == 1) then whole = whole + 1 end
  local suffix = deep_swe and " pass@1" or ""
  return string.format("%d/%d (%d%%)%s", ok, scored, whole, suffix)
end

local function run(style, argv)
  local args = common.parse_flags(argv, {
    ["--cells"] = "cells",
    ["--out-dir"] = "out_dir",
    ["--meta"] = "meta",
  }, { "cells", "out_dir" },
    "report.lua --cells CELLS --out-dir DIR [--meta JSON]")
  local cells = common.read_jsonl(args.cells)
  local meta = common.json.decode(args.meta or "{}")
  common.ensure_dir(args.out_dir)

  local by = {}
  for _, cell in ipairs(cells) do
    local key = cell.harness .. "\0" .. cell.model
    by[key] = by[key] or { harness = cell.harness, model = cell.model, rows = {} }
    table.insert(by[key].rows, cell)
  end
  local groups = {}
  for _, group in pairs(by) do groups[#groups + 1] = group end
  table.sort(groups, function(a, b)
    if a.harness == b.harness then return a.model < b.model end
    return a.harness < b.harness
  end)

  local deep_swe = style == "deep-swe"
  local title = deep_swe and "# DeepSWE accuracy" or "# SWE-bench Verified accuracy"
  local dataset = meta.dataset
  if dataset == nil then dataset = deep_swe and "datacurve/deep-swe" or "princeton-nlp/SWE-bench_Verified" end
  local metric = deep_swe
    and "- Metric: Pier verifier `reward.json` (reward==1). Not patch-byte size."
    or "- Metric: resolve rate (fail-to-pass tests), not wall-clock on toy tasks"
  local summary_header = deep_swe and "| harness | model | pass@1 | instances |" or "| harness | model | resolve | instances |"
  local lines = {
    title,
    "",
    "- Dataset: `" .. pystr(dataset) .. "`",
    metric,
    "- n=" .. pystr(meta.n) .. "  sample-seed=" .. pystr(meta.sample_seed),
  }
  if deep_swe then lines[#lines + 1] = "- Model: " .. pystr(meta.model) .. " effort=" .. pystr(meta.effort) end
  lines[#lines + 1] = "- Driver: " .. pystr(meta.driver or (deep_swe and "thin+pier-verifier" or "auto"))
  lines[#lines + 1] = "- Docker: " .. pystr(meta.docker or "unknown")
  lines[#lines + 1] = ""
  lines[#lines + 1] = "## Summary"
  lines[#lines + 1] = ""
  lines[#lines + 1] = summary_header
  lines[#lines + 1] = "|---|---|---|---|"
  for _, group in ipairs(groups) do
    lines[#lines + 1] = string.format("| %s | %s | %s | %d |", group.harness, group.model, rate(group.rows, deep_swe), #group.rows)
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = "## Cells"
  lines[#lines + 1] = ""
  lines[#lines + 1] = "| harness | model | instance_id | resolved | seconds |"
  lines[#lines + 1] = "|---|---|---|---|---|"
  for _, cell in ipairs(cells) do
    local resolved = cell.resolved
    local resolved_text = "null"
    if resolved == true then resolved_text = "true" end
    if resolved == false then resolved_text = "false" end
    lines[#lines + 1] = string.format("| %s | %s | %s | %s | %s |",
      cell.harness, cell.model, cell.instance_id, resolved_text, pystr(cell.seconds or ""))
  end
  lines[#lines + 1] = ""
  local report = table.concat(lines, "\n")
  common.write(args.out_dir .. "/REPORT.md", report)
  common.write_pretty(args.out_dir .. "/results.json", { meta = meta, cells = cells })
  io.stdout:write(report, "\n")
  return 0
end

M.run = run
return M
