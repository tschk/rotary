local common = require("common")
local M = {}

local function pystr(value)
  return common.python_string(value)
end

local function round_half_even(percent)
  local whole = math.floor(percent)
  local remainder = percent - whole
  if remainder > 0.5 or (remainder == 0.5 and whole % 2 == 1) then whole = whole + 1 end
  return whole
end

local function pct(k, n)
  return round_half_even(k / n * 100)
end

-- 95% Wilson score interval for k successes out of n trials.
local function wilson(k, n)
  local z = 1.959964
  local p = k / n
  local denom = 1 + z * z / n
  local center = (p + z * z / (2 * n)) / denom
  local half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / denom
  return string.format("%d–%d%%", round_half_even(math.max(0, center - half) * 100),
    round_half_even(math.min(1, center + half) * 100))
end

local function patch_bytes(cell)
  if type(cell.patch_bytes) == "number" then return cell.patch_bytes end
  if type(cell.patch) ~= "string" then return nil end
  local file = io.open(cell.patch, "rb")
  if not file then return nil end
  local size = file:seek("end")
  file:close()
  return size
end

-- Why a cell has no verdict. Every unscored cell still counts as unresolved.
local function unscored_reason(cell)
  if cell.missing then return "missing" end
  if type(cell.unscored_reason) == "string" then return cell.unscored_reason end
  if cell.error ~= nil then return tostring(cell.error) end
  if (cell.adapter_exit or 0) ~= 0 then return "adapter_exit_" .. tostring(cell.adapter_exit) end
  local bytes = patch_bytes(cell)
  if bytes == 0 then return "empty_patch" end
  if cell.eval_run_id == nil and cell.eval_attempted == nil then return "not_evaluated" end
  return "eval_no_result"
end

local function key(harness, model, instance)
  return harness .. "\0" .. model .. "\0" .. (instance or "")
end

local function read_ids(path)
  local ids = {}
  for line in (common.read(path) .. "\n"):gmatch("(.-)\n") do
    if line:match("%S") then ids[#ids + 1] = line:match("^%s*(.-)%s*$") end
  end
  return ids
end

-- Planned cells come from the plan file. Without one, the recorded cells are
-- the plan, so missing cells cannot be detected; the report says so.
local function build_plan(plan, cells)
  local pairs_, ids, excluded = {}, {}, {}
  if plan then
    pairs_ = plan.pairs or {}
    excluded = plan.excluded or {}
    if plan.instance_ids_file then ids = read_ids(plan.instance_ids_file) end
    if plan.instance_ids then ids = plan.instance_ids end
    local planned = {}
    for _, pair in ipairs(pairs_) do
      for _, id in ipairs(ids) do
        planned[#planned + 1] = { harness = pair.harness, model = pair.model, instance_id = id }
      end
    end
    return planned, excluded, #ids
  end
  local seen, planned = {}, {}
  for _, cell in ipairs(cells) do
    local k = key(cell.harness, cell.model, cell.instance_id)
    if not seen[k] then
      seen[k] = true
      planned[#planned + 1] = { harness = cell.harness, model = cell.model, instance_id = cell.instance_id }
    end
  end
  return planned, excluded, nil
end

local function run(style, argv)
  local args = common.parse_flags(argv, {
    ["--cells"] = "cells",
    ["--out-dir"] = "out_dir",
    ["--meta"] = "meta",
    ["--plan"] = "plan",
  }, { "cells", "out_dir" },
    "report.lua --cells CELLS --out-dir DIR [--meta JSON] [--plan PLAN.json]")
  local cells = common.read_jsonl(args.cells)
  local meta = common.json.decode(args.meta or "{}")
  local plan = args.plan and common.read_json(args.plan) or nil
  common.ensure_dir(args.out_dir)
  local deep_swe = style == "deep-swe"

  -- Index recorded cells; duplicates and unplanned cells are integrity issues.
  local recorded, issues = {}, {}
  for _, cell in ipairs(cells) do
    local k = key(cell.harness, cell.model, cell.instance_id)
    if recorded[k] then
      issues[#issues + 1] = string.format("duplicate cell %s/%s/%s (last one wins)", cell.harness, cell.model, cell.instance_id)
    end
    recorded[k] = cell
  end
  local planned, excluded, n_ids = build_plan(plan, cells)
  local in_plan = {}
  for _, p in ipairs(planned) do in_plan[key(p.harness, p.model, p.instance_id)] = true end
  for _, cell in ipairs(cells) do
    if not in_plan[key(cell.harness, cell.model, cell.instance_id)] then
      issues[#issues + 1] = string.format("unplanned cell %s/%s/%s (not counted)", cell.harness, cell.model, cell.instance_id)
    end
  end

  -- One row per planned cell, in plan order.
  local by, groups, rows = {}, {}, {}
  for _, p in ipairs(planned) do
    local cell = recorded[key(p.harness, p.model, p.instance_id)]
      or { harness = p.harness, model = p.model, instance_id = p.instance_id, missing = true }
    rows[#rows + 1] = cell
    local gk = key(p.harness, p.model)
    if not by[gk] then
      by[gk] = { harness = p.harness, model = p.model, rows = {}, ok = 0, scored = 0, reasons = {}, adapter_models = {}, adapter_failed = 0 }
      groups[#groups + 1] = by[gk]
    end
    local g = by[gk]
    g.rows[#g.rows + 1] = cell
    if cell.resolved == true or cell.resolved == false then
      g.scored = g.scored + 1
      if cell.resolved == true then g.ok = g.ok + 1 end
    else
      local reason = unscored_reason(cell)
      g.reasons[reason] = (g.reasons[reason] or 0) + 1
    end
    if not cell.missing and (cell.adapter_exit or 0) ~= 0 then g.adapter_failed = g.adapter_failed + 1 end
    if type(cell.adapter_model) == "string" then g.adapter_models[cell.adapter_model] = true end
  end
  table.sort(groups, function(a, b)
    if a.harness == b.harness then return a.model < b.model end
    return a.harness < b.harness
  end)

  local title = deep_swe and "# DeepSWE accuracy" or "# SWE-bench Verified accuracy"
  local dataset = meta.dataset
  if dataset == nil then dataset = deep_swe and "datacurve/deep-swe" or "princeton-nlp/SWE-bench_Verified" end
  local metric = deep_swe
    and "- Metric: pass@1 = Pier verifier `reward.json` reward==1 / planned tasks. Not patch-byte size."
    or "- Metric: resolve rate = official-eval resolved / planned instances (fail-to-pass tests)"
  local lines = {
    title,
    "",
    "- Dataset: `" .. pystr(dataset) .. "`",
    metric,
    "- Denominator: every planned cell. Unscored and missing cells count as unresolved;"
      .. " `upper` is the rate if every unscored cell had resolved. Rates are only comparable"
      .. " when status is `complete`.",
    "- n=" .. pystr(meta.n) .. "  sample-seed=" .. pystr(meta.sample_seed),
  }
  if deep_swe then lines[#lines + 1] = "- Model: " .. pystr(meta.model) .. " effort=" .. pystr(meta.effort) end
  lines[#lines + 1] = "- Driver: " .. pystr(meta.driver or (deep_swe and "thin+pier-verifier" or "auto"))
  lines[#lines + 1] = "- Docker: " .. pystr(meta.docker or "unknown")
  local prov = meta.provenance
  if type(prov) == "table" then
    lines[#lines + 1] = "- Dataset revision: " .. pystr(prov.dataset_revision or "unrecorded")
    lines[#lines + 1] = string.format("- Provenance: rotary `%s`%s, models.json sha256 `%s`, instance set sha256 `%s`, %s → %s",
      pystr(prov.git_commit), prov.git_dirty and " (dirty tree)" or "", pystr(prov.models_sha256),
      pystr(prov.instance_ids_sha256), pystr(prov.started_utc), pystr(meta.finished_utc))
  end
  if not plan then
    lines[#lines + 1] = "- Plan: none given; coverage is relative to recorded cells, so never-run cells are invisible."
  else
    lines[#lines + 1] = string.format("- Plan: %d harness×model pairs × %d instances = %d cells; %d pairs excluded",
      #(plan.pairs or {}), n_ids or 0, #planned, #excluded)
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = "## Summary"
  lines[#lines + 1] = ""
  lines[#lines + 1] = string.format("| harness | model | %s | 95%% CI | upper | scored | status | adapter model |",
    deep_swe and "pass@1" or "resolve")
  lines[#lines + 1] = "|---|---|---|---|---|---|---|---|"
  for _, g in ipairs(groups) do
    local n = #g.rows
    local adapter_models = {}
    for m in pairs(g.adapter_models) do adapter_models[#adapter_models + 1] = m end
    table.sort(adapter_models)
    if #adapter_models > 1 then
      issues[#issues + 1] = string.format("%s/%s ran under several adapter models: %s", g.harness, g.model, table.concat(adapter_models, ", "))
    end
    if g.adapter_failed > 0 then
      issues[#issues + 1] = string.format("%s/%s: %d/%d cells exited non-zero in the adapter; those count as unresolved and may measure harness failure, not the model",
        g.harness, g.model, g.adapter_failed, n)
    end
    local rate, ci, upper, status
    if g.scored == 0 then
      rate, ci, upper, status = "n/a (" .. (deep_swe and "no Pier verifier reward.json" or "no official eval") .. ")", "—", "100%", "NOT SCORED"
    else
      rate = string.format("%d/%d (%d%%)", g.ok, n, pct(g.ok, n))
      upper = string.format("%d%%", pct(g.ok + n - g.scored, n))
      status = g.scored == n and "complete" or "INCOMPLETE"
      -- An interval around a partly unscored rate would look like precision it lacks.
      ci = status == "complete" and wilson(g.ok, n) or "—"
    end
    lines[#lines + 1] = string.format("| %s | %s | %s | %s | %s | %d/%d (%d%%) | %s | %s |",
      g.harness, g.model, rate, ci, upper, g.scored, n, pct(g.scored, n), status,
      #adapter_models > 0 and table.concat(adapter_models, ", ") or "unrecorded")
  end

  lines[#lines + 1] = ""
  lines[#lines + 1] = "## Exclusions"
  lines[#lines + 1] = ""
  lines[#lines + 1] = "Pairs not run (no cells, not in the denominator):"
  lines[#lines + 1] = ""
  lines[#lines + 1] = "| harness | model | reason |"
  lines[#lines + 1] = "|---|---|---|"
  for _, e in ipairs(excluded) do
    lines[#lines + 1] = string.format("| %s | %s | %s |", e.harness, e.model, pystr(e.reason))
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = "Unscored cells (in the denominator as unresolved):"
  lines[#lines + 1] = ""
  lines[#lines + 1] = "| harness | model | reason | cells |"
  lines[#lines + 1] = "|---|---|---|---|"
  for _, g in ipairs(groups) do
    local reasons = {}
    for r in pairs(g.reasons) do reasons[#reasons + 1] = r end
    table.sort(reasons)
    for _, r in ipairs(reasons) do
      lines[#lines + 1] = string.format("| %s | %s | %s | %d |", g.harness, g.model, r, g.reasons[r])
    end
  end
  lines[#lines + 1] = ""
  lines[#lines + 1] = "Integrity issues:"
  lines[#lines + 1] = ""
  if #issues == 0 then lines[#lines + 1] = "- none" end
  for _, issue in ipairs(issues) do lines[#lines + 1] = "- " .. issue end

  lines[#lines + 1] = ""
  lines[#lines + 1] = "## Tokens and cost coverage"
  lines[#lines + 1] = ""
  lines[#lines + 1] = "Missing usage/cost is unknown, not zero. Costs are harness estimates or explicit dated rate snapshots, not invoices."
  lines[#lines + 1] = ""
  lines[#lines + 1] = "| harness | model | complete usage/planned | priced/planned | known USD subtotal |"
  lines[#lines + 1] = "|---|---|---|---|---|"
  for _, g in ipairs(groups) do
    local known, priced, usd = 0, 0, 0
    for _, cell in ipairs(g.rows) do
      local e = cell.execution
      if type(e) == "table" and e ~= common.json.null and e.usage_complete == true then
        known = known + 1
        if type(e.cost_usd) == "number" then priced = priced + 1; usd = usd + e.cost_usd end
      end
    end
    lines[#lines + 1] = string.format("| %s | %s | %d/%d | %d/%d | %s |",
      g.harness, g.model, known, #g.rows, priced, #g.rows, priced > 0 and string.format("%.6f", usd) or "unknown")
  end

  lines[#lines + 1] = ""
  lines[#lines + 1] = "## Cells"
  lines[#lines + 1] = ""
  lines[#lines + 1] = "| harness | model | instance_id | resolved | seconds | unscored reason | input/output/cache-read/cache-write | USD estimate | usage complete |"
  lines[#lines + 1] = "|---|---|---|---|---|---|---|---|---|"
  for _, cell in ipairs(rows) do
    local resolved_text, reason = "null", unscored_reason(cell)
    if cell.missing then resolved_text = "missing" end
    if cell.resolved == true then resolved_text, reason = "true", "" end
    if cell.resolved == false then resolved_text, reason = "false", "" end
    local execution = cell.execution or {}
    local tokens = type(execution.tokens) == "table" and execution.tokens ~= common.json.null and execution.tokens or nil
    local token_text = tokens and string.format("%s/%s/%s/%s", tokens.input, tokens.output, tokens.cache_read, tokens.cache_write) or "unknown"
    local cost = type(execution.cost_usd) == "number" and string.format("%.6f", execution.cost_usd) or "unknown"
    lines[#lines + 1] = string.format("| %s | %s | %s | %s | %s | %s | %s | %s | %s |",
      cell.harness, cell.model, cell.instance_id, resolved_text, pystr(cell.seconds or ""), reason,
      token_text, cost, execution.usage_complete == true and "yes" or "no")
  end
  lines[#lines + 1] = ""
  local report = table.concat(lines, "\n")
  common.write(args.out_dir .. "/REPORT.md", report)
  common.write_pretty(args.out_dir .. "/results.json", { meta = meta, plan = plan or common.json.null, cells = cells })
  io.stdout:write(report, "\n")
  return 0
end

M.run = run
return M
