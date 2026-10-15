#!/usr/bin/env luajit
local source = debug.getinfo(1, "S").source:sub(2)
local dir = source:match("^(.*)/") or "."
package.path = dir .. "/?.lua;" .. package.path
os.exit(require("report_core").run("verified", arg))
