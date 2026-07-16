const std = @import("std");
const agent = @import("agent.zig");

const log = std.log.scoped(.plugin);

pub const PluginId = []const u8;

pub const RegisteredTool = struct {
    name: []const u8,
    description: []const u8,
    parameters_json: []const u8,
    plugin_id: []const u8,
};

pub const RegisteredCommand = struct {
    name: []const u8,
    description: []const u8,
    plugin_id: []const u8,
};

pub const HostMessage = struct {
    type: []const u8,
    name: ?[]const u8 = null,
    description: ?[]const u8 = null,
    parameters: ?std.json.Value = null,
    event: ?[]const u8 = null,
    data: ?[]const u8 = null,
    id: ?[]const u8 = null,
    arguments: ?[]const u8 = null,
    args: ?[]const u8 = null,
    content: ?[]const u8 = null,
    is_error: ?bool = null,
    result: ?[]const u8 = null,
    method: ?[]const u8 = null,
    title: ?[]const u8 = null,
    message: ?[]const u8 = null,
    options: ?std.json.Value = null,
    value: ?[]const u8 = null,
    model: ?[]const u8 = null,
    level: ?[]const u8 = null,
    tools: ?std.json.Value = null,
    entry_type: ?[]const u8 = null,
    placeholder: ?[]const u8 = null,
    prefill: ?[]const u8 = null,
    status_key: ?[]const u8 = null,
    status_text: ?[]const u8 = null,
    widget_key: ?[]const u8 = null,
    widget_lines: ?std.json.Value = null,
    widget_placement: ?[]const u8 = null,
    text: ?[]const u8 = null,
    notify_type: ?[]const u8 = null,
    confirmed: ?bool = null,
    cancelled: ?bool = null,
};

pub const Plugin = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    id: []const u8,
    name: []const u8,
    path: []const u8,
    shim_path: []const u8,
    process: ?std.process.Child = null,
    read_buf: []u8 = &.{},
    stdout_reader: ?std.Io.File.Reader = null,
    next_rpc_id: u64 = 1,
    ready: bool = false,
    registered_tools: std.ArrayList(RegisteredTool),
    registered_commands: std.ArrayList(RegisteredCommand),
    pending_tool_results: std.AutoHashMap(u64, PendingToolResult),
    registry: ?*Registry = null,

    pub const PendingToolResult = struct {
        content: []const u8,
        is_error: bool,
    };

    pub fn init(allocator: std.mem.Allocator, io: std.Io, id: []const u8, name: []const u8, path: []const u8) !Plugin {
        return .{
            .allocator = allocator,
            .io = io,
            .id = try allocator.dupe(u8, id),
            .name = try allocator.dupe(u8, name),
            .path = try allocator.dupe(u8, path),
            .shim_path = try allocator.dupe(u8, "plugins/shim.ts"),
            .registered_tools = std.ArrayList(RegisteredTool).empty,
            .registered_commands = std.ArrayList(RegisteredCommand).empty,
            .pending_tool_results = std.AutoHashMap(u64, PendingToolResult).init(allocator),
        };
    }

    pub fn deinit(self: *Plugin) void {
        self.kill();
        self.allocator.free(self.id);
        self.allocator.free(self.name);
        self.allocator.free(self.path);
        self.allocator.free(self.shim_path);
        if (self.read_buf.len > 0) self.allocator.free(self.read_buf);
        for (self.registered_tools.items) |tool| {
            self.allocator.free(tool.name);
            self.allocator.free(tool.description);
            self.allocator.free(tool.parameters_json);
            self.allocator.free(tool.plugin_id);
        }
        self.registered_tools.deinit(self.allocator);
        for (self.registered_commands.items) |cmd| {
            self.allocator.free(cmd.name);
            self.allocator.free(cmd.description);
            self.allocator.free(cmd.plugin_id);
        }
        self.registered_commands.deinit(self.allocator);

        var it = self.pending_tool_results.iterator();
        while (it.next()) |entry| {
            self.allocator.free(entry.value_ptr.content);
        }
        self.pending_tool_results.deinit();
    }

    pub fn start(self: *Plugin) !void {
        self.process = try std.process.spawn(self.io, .{
            .argv = &.{ "bun", "run", self.shim_path, self.path },
            .stdin = .pipe,
            .stdout = .pipe,
            .stderr = .inherit,
        });
        log.info("started plugin {s}: bun run {s} {s}", .{ self.id, self.shim_path, self.path });

        self.read_buf = try self.allocator.alloc(u8, 8192);
        const stdout_file = self.process.?.stdout orelse return error.PluginNotRunning;
        self.stdout_reader = std.Io.File.Reader.init(stdout_file, self.io, self.read_buf);

        try self.sendInit();
    }

    pub fn kill(self: *Plugin) void {
        if (self.process) |*p| {
            p.kill(self.io);
            self.process = null;
        }
    }

    fn sendJsonl(self: *Plugin, msg: anytype) !void {
        if (self.process == null) return error.PluginNotRunning;
        const file = self.process.?.stdin orelse return error.PluginNotRunning;

        var buf: [8192]u8 = undefined;
        var file_writer: std.Io.File.Writer = .init(file, self.io, &buf);
        const writer = &file_writer.interface;

        try std.json.Stringify.value(msg, .{}, writer);
        try writer.writeAll("\n");
        try writer.flush();
    }

    fn sendInit(self: *Plugin) !void {
        try self.sendJsonl(.{
            .type = "init",
            .path = self.path,
        });
    }

    pub fn sendEvent(self: *Plugin, event_name: []const u8, data_json: ?[]const u8) !void {
        try self.sendJsonl(.{
            .type = "event",
            .event = event_name,
            .data = data_json,
        });
    }

    pub fn callTool(self: *Plugin, call_id: u64, tool_name: []const u8, arguments_json: []const u8) !void {
        var id_buf: [32]u8 = undefined;
        const id_str = try std.fmt.bufPrint(&id_buf, "{d}", .{call_id});

        try self.sendJsonl(.{
            .type = "tool_call",
            .id = id_str,
            .name = tool_name,
            .arguments = arguments_json,
        });
    }

    pub fn sendCommand(self: *Plugin, cmd_name: []const u8, args: []const u8) !void {
        try self.sendJsonl(.{
            .type = "command",
            .name = cmd_name,
            .args = args,
        });
    }

    fn sendUiResponse(self: *Plugin, resp: UiResponse) !void {
        try self.sendJsonl(.{
            .type = "ui_response",
            .id = resp.id,
            .confirmed = resp.confirmed,
            .cancelled = resp.cancelled,
            .value = resp.value,
        });
    }

    pub fn nextId(self: *Plugin) u64 {
        const id = self.next_rpc_id;
        self.next_rpc_id += 1;
        return id;
    }

    pub fn readMessage(self: *Plugin, arena: std.mem.Allocator) !?HostMessage {
        if (self.process == null) return null;
        if (self.stdout_reader == null) return null;
        const reader = &self.stdout_reader.?.interface;

        const line = reader.takeDelimiter('\n') catch |err| {
            if (err == error.EndOfStream) return null;
            return err;
        };
        if (line == null) return null;

        const parsed = std.json.parseFromSlice(HostMessage, arena, line.?, .{
            .ignore_unknown_fields = true,
        }) catch |err| {
            log.err("plugin {s}: failed to parse message: {}", .{ self.id, err });
            return null;
        };

        return parsed.value;
    }

    pub fn tryReadMessage(self: *Plugin, arena: std.mem.Allocator) !?HostMessage {
        if (self.process == null) return null;
        if (self.stdout_reader == null) return null;
        const reader = &self.stdout_reader.?.interface;

        if (reader.bufferedLen() == 0) return null;

        return self.readMessage(arena);
    }

    pub fn processMessage(self: *Plugin, msg: HostMessage) !void {
        if (std.mem.eql(u8, msg.type, "shim_loaded")) {
            log.info("plugin {s}: shim loaded", .{self.id});
            return;
        }

        if (std.mem.eql(u8, msg.type, "ready")) {
            self.ready = true;
            log.info("plugin {s}: ready", .{self.id});
            return;
        }

        if (std.mem.eql(u8, msg.type, "register_tool")) {
            if (msg.name) |name| {
                var params_buf: std.Io.Writer.Allocating = .init(self.allocator);
                const pw = &params_buf.writer;
                defer params_buf.deinit();

                if (msg.parameters) |params| {
                    try std.json.Stringify.value(params, .{}, pw);
                } else {
                    try pw.writeAll("{}");
                }

                try self.registered_tools.append(self.allocator, .{
                    .name = try self.allocator.dupe(u8, name),
                    .description = try self.allocator.dupe(u8, msg.description orelse ""),
                    .parameters_json = try self.allocator.dupe(u8, params_buf.written()),
                    .plugin_id = try self.allocator.dupe(u8, self.id),
                });
                log.info("plugin {s} registered tool: {s}", .{ self.id, name });
            }
            return;
        }

        if (std.mem.eql(u8, msg.type, "register_command")) {
            if (msg.name) |name| {
                try self.registered_commands.append(self.allocator, .{
                    .name = try self.allocator.dupe(u8, name),
                    .description = try self.allocator.dupe(u8, msg.description orelse ""),
                    .plugin_id = try self.allocator.dupe(u8, self.id),
                });
                log.info("plugin {s} registered command: {s}", .{ self.id, name });
            }
            return;
        }

        if (std.mem.eql(u8, msg.type, "tool_result")) {
            if (msg.id) |id_str| {
                const call_id = std.fmt.parseInt(u64, id_str, 10) catch {
                    log.err("plugin {s}: invalid tool result id: {s}", .{ self.id, id_str });
                    return;
                };
                const content = try self.allocator.dupe(u8, msg.content orelse "");
                try self.pending_tool_results.put(call_id, .{
                    .content = content,
                    .is_error = msg.is_error orelse false,
                });
                log.info("plugin {s}: tool result for {d}", .{ self.id, call_id });
            }
            return;
        }

        if (std.mem.eql(u8, msg.type, "error")) {
            log.err("plugin {s} error: {s}", .{ self.id, msg.message orelse "unknown" });
            return;
        }

        if (std.mem.eql(u8, msg.type, "ui_request")) {
            if (self.registry) |reg| {
                const req = UiRequest{
                    .id = msg.id orelse "",
                    .method = msg.method orelse "",
                    .title = msg.title,
                    .message = msg.message,
                    .options = msg.options,
                    .placeholder = msg.placeholder,
                    .prefill = msg.prefill,
                    .status_key = msg.status_key,
                    .status_text = msg.status_text,
                    .widget_key = msg.widget_key,
                    .widget_lines = msg.widget_lines,
                    .widget_placement = msg.widget_placement,
                    .text = msg.text,
                    .notify_type = msg.notify_type,
                };
                if (reg.handleUiRequest(self.id, req)) |resp| {
                    self.sendUiResponse(resp) catch {};
                }
            }
            return;
        }

        if (std.mem.eql(u8, msg.type, "event_result")) {
            log.info("plugin {s}: event_result for {s}", .{ self.id, msg.event orelse "?" });
            return;
        }

        if (std.mem.eql(u8, msg.type, "send_message")) {
            if (self.registry) |reg| {
                reg.dispatchAction(self.id, .{ .send_message = .{
                    .message = msg.message orelse "",
                    .options = msg.options,
                } });
            }
            return;
        }

        if (std.mem.eql(u8, msg.type, "send_user_message")) {
            if (self.registry) |reg| {
                reg.dispatchAction(self.id, .{ .send_user_message = .{
                    .content = msg.content orelse "",
                    .options = msg.options,
                } });
            }
            return;
        }

        if (std.mem.eql(u8, msg.type, "append_entry")) {
            if (self.registry) |reg| {
                reg.dispatchAction(self.id, .{ .append_entry = .{
                    .entry_type = msg.entry_type orelse "",
                    .data = msg.data,
                } });
            }
            return;
        }

        if (std.mem.eql(u8, msg.type, "set_session_name")) {
            if (self.registry) |reg| {
                reg.dispatchAction(self.id, .{ .set_session_name = msg.name orelse "" });
            }
            return;
        }

        if (std.mem.eql(u8, msg.type, "set_model")) {
            if (self.registry) |reg| {
                reg.dispatchAction(self.id, .{ .set_model = msg.model orelse "" });
            }
            return;
        }

        if (std.mem.eql(u8, msg.type, "set_thinking_level")) {
            if (self.registry) |reg| {
                reg.dispatchAction(self.id, .{ .set_thinking_level = msg.level orelse "" });
            }
            return;
        }

        if (std.mem.eql(u8, msg.type, "set_active_tools")) {
            if (self.registry) |reg| {
                var tools_buf: std.Io.Writer.Allocating = .init(self.allocator);
                const tw = &tools_buf.writer;
                defer tools_buf.deinit();
                if (msg.tools) |tools| {
                    std.json.Stringify.value(tools, .{}, tw) catch {};
                }
                reg.dispatchAction(self.id, .{ .set_active_tools = self.allocator.dupe(u8, tools_buf.written()) catch "" });
            }
            return;
        }

        log.info("plugin {s}: unhandled message type: {s}", .{ self.id, msg.type });
    }

    pub fn getToolResult(self: *Plugin, call_id: u64) ?PendingToolResult {
        return self.pending_tool_results.get(call_id);
    }

    pub fn consumeToolResult(self: *Plugin, call_id: u64) ?PendingToolResult {
        const result = self.pending_tool_results.get(call_id);
        if (result != null) {
            _ = self.pending_tool_results.remove(call_id);
        }
        return result;
    }

    pub fn pumpMessages(self: *Plugin, arena: std.mem.Allocator) !void {
        while (true) {
            const msg = try self.readMessage(arena);
            if (msg == null) break;
            try self.processMessage(msg.?);
        }
    }

    pub fn tryPumpMessages(self: *Plugin, arena: std.mem.Allocator) !void {
        while (true) {
            const msg = try self.tryReadMessage(arena);
            if (msg == null) break;
            try self.processMessage(msg.?);
        }
    }
};

pub const Action = union(enum) {
    send_message: struct { message: []const u8, options: ?std.json.Value = null },
    send_user_message: struct { content: []const u8, options: ?std.json.Value = null },
    append_entry: struct { entry_type: []const u8, data: ?[]const u8 = null },
    set_session_name: []const u8,
    set_model: []const u8,
    set_thinking_level: []const u8,
    set_active_tools: []const u8,
};

pub const UiRequest = struct {
    id: []const u8,
    method: []const u8,
    title: ?[]const u8 = null,
    message: ?[]const u8 = null,
    options: ?std.json.Value = null,
    placeholder: ?[]const u8 = null,
    prefill: ?[]const u8 = null,
    status_key: ?[]const u8 = null,
    status_text: ?[]const u8 = null,
    widget_key: ?[]const u8 = null,
    widget_lines: ?std.json.Value = null,
    widget_placement: ?[]const u8 = null,
    text: ?[]const u8 = null,
    notify_type: ?[]const u8 = null,
};

pub const UiResponse = struct {
    id: []const u8,
    confirmed: ?bool = null,
    cancelled: ?bool = null,
    value: ?[]const u8 = null,
};

pub const UiHandler = *const fn (ctx: ?*anyopaque, plugin_id: []const u8, req: UiRequest) UiResponse;

pub const ActionHandler = *const fn (ctx: ?*anyopaque, plugin_id: []const u8, action: Action) void;

pub const Registry = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    plugins: std.ArrayList(Plugin),
    action_ctx: ?*anyopaque = null,
    action_handler: ?ActionHandler = null,
    ui_ctx: ?*anyopaque = null,
    ui_handler: ?UiHandler = null,

    pub fn init(allocator: std.mem.Allocator, io: std.Io) Registry {
        return .{
            .allocator = allocator,
            .io = io,
            .plugins = std.ArrayList(Plugin).empty,
        };
    }

    pub fn deinit(self: *Registry) void {
        for (self.plugins.items) |*plugin| {
            plugin.deinit();
        }
        self.plugins.deinit(self.allocator);
    }

    pub fn setActionHandler(self: *Registry, ctx: ?*anyopaque, handler: ActionHandler) void {
        self.action_ctx = ctx;
        self.action_handler = handler;
    }

    pub fn dispatchAction(self: *Registry, plugin_id: []const u8, action: Action) void {
        if (self.action_handler) |handler| {
            handler(self.action_ctx, plugin_id, action);
        }
    }

    pub fn setUiHandler(self: *Registry, ctx: ?*anyopaque, handler: UiHandler) void {
        self.ui_ctx = ctx;
        self.ui_handler = handler;
    }

    pub fn handleUiRequest(self: *Registry, plugin_id: []const u8, req: UiRequest) ?UiResponse {
        if (self.ui_handler) |handler| {
            return handler(self.ui_ctx, plugin_id, req);
        }
        return null;
    }

    pub fn add(self: *Registry, id: []const u8, name: []const u8, path: []const u8) !void {
        var plugin = try Plugin.init(self.allocator, self.io, id, name, path);
        plugin.registry = self;
        try self.plugins.append(self.allocator, plugin);
        log.info("loaded plugin: {s}", .{id});
    }

    pub fn startAll(self: *Registry) !void {
        for (self.plugins.items) |*plugin| {
            try plugin.start();
        }
    }

    pub fn stopAll(self: *Registry) void {
        for (self.plugins.items) |*plugin| {
            plugin.kill();
        }
    }

    pub fn pumpAll(self: *Registry, arena: std.mem.Allocator) !void {
        for (self.plugins.items) |*plugin| {
            try plugin.pumpMessages(arena);
        }
    }

    pub fn tryPumpAll(self: *Registry, arena: std.mem.Allocator) !void {
        for (self.plugins.items) |*plugin| {
            try plugin.tryPumpMessages(arena);
        }
    }

    pub fn forwardEvent(self: *Registry, event_name: []const u8, data_json: ?[]const u8) !void {
        for (self.plugins.items) |*plugin| {
            if (plugin.ready) {
                plugin.sendEvent(event_name, data_json) catch |err| {
                    log.err("failed to forward event {s} to plugin {s}: {}", .{
                        event_name,
                        plugin.id,
                        err,
                    });
                };
            }
        }
    }

    pub fn count(self: *const Registry) usize {
        return self.plugins.items.len;
    }

    pub fn get(self: *const Registry, id: []const u8) ?*const Plugin {
        for (self.plugins.items) |*plugin| {
            if (std.mem.eql(u8, plugin.id, id)) return plugin;
        }
        return null;
    }

    pub fn getMut(self: *Registry, id: []const u8) ?*Plugin {
        for (self.plugins.items) |*plugin| {
            if (std.mem.eql(u8, plugin.id, id)) return plugin;
        }
        return null;
    }

    pub fn allTools(self: *const Registry, allocator: std.mem.Allocator) ![]RegisteredTool {
        var result: std.ArrayList(RegisteredTool) = .empty;
        for (self.plugins.items) |plugin| {
            for (plugin.registered_tools.items) |tool| {
                try result.append(allocator, tool);
            }
        }
        return result.toOwnedSlice(allocator);
    }

    pub fn findTool(self: *const Registry, name: []const u8) ?RegisteredTool {
        for (self.plugins.items) |plugin| {
            for (plugin.registered_tools.items) |tool| {
                if (std.mem.eql(u8, tool.name, name)) return tool;
            }
        }
        return null;
    }

    pub fn findToolPlugin(self: *Registry, name: []const u8) ?*Plugin {
        for (self.plugins.items) |*plugin| {
            for (plugin.registered_tools.items) |tool| {
                if (std.mem.eql(u8, tool.name, name)) return plugin;
            }
        }
        return null;
    }
};

pub const Skill = struct {
    name: []const u8,
    description: []const u8,
    path: []const u8,
    disable_model_invocation: bool = false,
};

pub const SkillLoader = struct {
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator) SkillLoader {
        return .{ .allocator = allocator };
    }

    pub fn loadFromDir(self: *SkillLoader, dir_path: []const u8) ![]Skill {
        var skills: std.ArrayList(Skill) = .empty;
        self.scanDir(&skills, dir_path) catch |err| {
            log.warn("skill dir {s}: {}", .{ dir_path, err });
        };
        return skills.toOwnedSlice(self.allocator);
    }

    fn scanDir(self: *SkillLoader, skills: *std.ArrayList(Skill), dir_path: []const u8) !void {
        const io = std.Io.Threaded.global_single_threaded.io();
        var dir = std.Io.Dir.cwd().openDir(io, dir_path, .{ .iterate = true }) catch return;
        defer dir.close(io);

        var iter = dir.iterate();
        while (try iter.next(io)) |entry| {
            if (entry.kind == .file and std.mem.eql(u8, entry.name, "SKILL.md")) {
                const skill = try self.parseSkillFile(dir_path, entry.name);
                try skills.append(self.allocator, skill);
            } else if (entry.kind == .directory) {
                const sub_path = try std.fmt.allocPrint(self.allocator, "{s}/{s}", .{ dir_path, entry.name });
                defer self.allocator.free(sub_path);
                const skill_file = try std.fmt.allocPrint(self.allocator, "{s}/SKILL.md", .{sub_path});
                defer self.allocator.free(skill_file);
                if (std.Io.Dir.cwd().access(io, skill_file, .{})) {
                    try skills.append(self.allocator, try self.parseSkillFile(sub_path, "SKILL.md"));
                } else |_| {
                    try self.scanDir(skills, sub_path);
                }
            }
        }
    }

    fn parseSkillFile(self: *SkillLoader, dir_path: []const u8, filename: []const u8) !Skill {
        const io = std.Io.Threaded.global_single_threaded.io();
        const full_path = try std.fmt.allocPrint(self.allocator, "{s}/{s}", .{ dir_path, filename });
        const content = try std.Io.Dir.cwd().readFileAlloc(io, full_path, self.allocator, .limited(1024 * 1024));
        defer self.allocator.free(content);

        var name: []const u8 = try self.allocator.dupe(u8, "unnamed");
        var description: []const u8 = try self.allocator.dupe(u8, "");
        var disable = false;

        if (std.mem.startsWith(u8, content, "---")) {
            const end = std.mem.indexOf(u8, content[3..], "---") orelse return .{
                .name = name,
                .description = description,
                .path = full_path,
                .disable_model_invocation = disable,
            };
            const frontmatter = content[3 .. 3 + end];
            var line_iter = std.mem.splitScalar(u8, frontmatter, '\n');
            while (line_iter.next()) |line| {
                const trimmed = std.mem.trim(u8, line, " \r\t");
                if (std.mem.startsWith(u8, trimmed, "name:")) {
                    self.allocator.free(name);
                    name = try self.allocator.dupe(u8, std.mem.trim(u8, trimmed[5..], " "));
                } else if (std.mem.startsWith(u8, trimmed, "description:")) {
                    self.allocator.free(description);
                    description = try self.allocator.dupe(u8, std.mem.trim(u8, trimmed[12..], " "));
                } else if (std.mem.startsWith(u8, trimmed, "disable-model-invocation:")) {
                    const val = std.mem.trim(u8, trimmed[25..], " ");
                    disable = std.mem.eql(u8, val, "true");
                }
            }
        }

        return .{
            .name = name,
            .description = description,
            .path = full_path,
            .disable_model_invocation = disable,
        };
    }

    pub fn buildSystemPrompt(self: *SkillLoader, skills: []const Skill) ![]u8 {
        var buf: std.Io.Writer.Allocating = .init(self.allocator);
        const writer = &buf.writer;
        defer buf.deinit();

        try writer.writeAll("<available_skills>\n");
        for (skills) |skill| {
            try writer.print(
                \\  <skill>
                \\    <name>{s}</name>
                \\    <description>{s}</description>
                \\    <location>{s}</location>
                \\  </skill>
                \\
            , .{ skill.name, skill.description, skill.path });
        }
        try writer.writeAll("</available_skills>");
        return try self.allocator.dupe(u8, buf.written());
    }
};

pub const PluginToolBridge = struct {
    registry: *Registry,
    arena: std.mem.Allocator,
    contexts: std.ArrayList(*ToolExecCtx),

    pub const ToolExecCtx = struct {
        plugin: *Plugin,
        tool_name: []const u8,
    };

    pub fn init(registry: *Registry, arena: std.mem.Allocator) PluginToolBridge {
        return .{
            .registry = registry,
            .arena = arena,
            .contexts = .empty,
        };
    }

    pub fn deinit(self: *PluginToolBridge) void {
        for (self.contexts.items) |ctx| {
            self.arena.free(ctx.tool_name);
            self.arena.destroy(ctx);
        }
        self.contexts.deinit(self.arena);
    }

    pub fn registerPluginToolsToAgent(self: *PluginToolBridge, tool_registry: *agent.ToolRegistry) !void {
        for (self.registry.plugins.items) |*plugin| {
            for (plugin.registered_tools.items) |reg_tool| {
                const ctx = try self.arena.create(ToolExecCtx);
                ctx.* = .{
                    .plugin = plugin,
                    .tool_name = try self.arena.dupe(u8, reg_tool.name),
                };
                try self.contexts.append(self.arena, ctx);

                const tool_def = agent.ToolDefinition{
                    .name = reg_tool.name,
                    .description = reg_tool.description,
                    .parameters_json = reg_tool.parameters_json,
                    .execute = executePluginTool,
                    .ctx = @ptrCast(ctx),
                };
                try tool_registry.register(tool_def);
            }
        }
    }

    fn executePluginTool(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) anyerror!agent.ToolResult {
        const exec_ctx: *ToolExecCtx = @ptrCast(@alignCast(ctx.?));
        const plugin = exec_ctx.plugin;
        const call_id = plugin.nextId();

        try plugin.callTool(call_id, exec_ctx.tool_name, arguments);

        var arena = std.heap.ArenaAllocator.init(allocator);
        defer arena.deinit();
        while (true) {
            const msg = try plugin.readMessage(arena.allocator());
            if (msg == null) break;
            try plugin.processMessage(msg.?);

            if (plugin.consumeToolResult(call_id)) |result| {
                return .{
                    .id = "plugin-tool",
                    .content = result.content,
                    .is_error = result.is_error,
                };
            }
        }

        return .{
            .id = "plugin-tool",
            .content = try allocator.dupe(u8, "[plugin tool returned no result]"),
            .is_error = true,
        };
    }
};

test "plugin registry loads plugin" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();
    var registry = Registry.init(gpa, io);
    defer registry.deinit();
    try registry.add("test", "Test Plugin", "./plugins/test.ts");
    try std.testing.expectEqual(@as(usize, 1), registry.count());
    try std.testing.expect(registry.get("test") != null);
}

test "plugin register tool locally" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();
    var plugin = try Plugin.init(gpa, io, "test", "Test", "./test.ts");
    defer plugin.deinit();

    try plugin.registered_tools.append(gpa, .{
        .name = try gpa.dupe(u8, "read_file"),
        .description = try gpa.dupe(u8, "Read a file"),
        .parameters_json = try gpa.dupe(u8, "{}"),
        .plugin_id = try gpa.dupe(u8, "test"),
    });
    try std.testing.expectEqual(@as(usize, 1), plugin.registered_tools.items.len);
    try std.testing.expectEqualStrings("read_file", plugin.registered_tools.items[0].name);
}

test "registry find tool across plugins" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();
    var registry = Registry.init(gpa, io);
    defer registry.deinit();

    try registry.add("p1", "Plugin 1", "./p1.ts");
    var p1 = registry.getMut("p1").?;
    try p1.registered_tools.append(gpa, .{
        .name = try gpa.dupe(u8, "word_count"),
        .description = try gpa.dupe(u8, "Count words"),
        .parameters_json = try gpa.dupe(u8, "{}"),
        .plugin_id = try gpa.dupe(u8, "p1"),
    });

    const found = registry.findTool("word_count");
    try std.testing.expect(found != null);
    try std.testing.expectEqualStrings("word_count", found.?.name);

    try std.testing.expect(registry.findTool("nonexistent") == null);
}

test "skill loader parses frontmatter" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();
    const tmp_dir = ".rotary-test-skills";
    defer std.Io.Dir.cwd().deleteTree(io, tmp_dir) catch {};

    try std.Io.Dir.cwd().createDirPath(io, tmp_dir);
    const skill_content =
        \\---
        \\name: test-skill
        \\description: A test skill
        \\disable-model-invocation: true
        \\---
        \\# Test Skill
        \\This is a test.
    ;
    const skill_path = try std.fmt.allocPrint(gpa, "{s}/SKILL.md", .{tmp_dir});
    defer gpa.free(skill_path);
    const file = try std.Io.Dir.cwd().createFile(io, skill_path, .{});
    defer file.close(io);
    try file.writeStreamingAll(io, skill_content);

    var loader = SkillLoader.init(gpa);
    const skills = try loader.loadFromDir(tmp_dir);
    defer {
        for (skills) |s| {
            gpa.free(s.name);
            gpa.free(s.description);
            gpa.free(s.path);
        }
        gpa.free(skills);
    }

    try std.testing.expectEqual(@as(usize, 1), skills.len);
    try std.testing.expectEqualStrings("test-skill", skills[0].name);
    try std.testing.expect(skills[0].disable_model_invocation);
}

test "skill loader builds system prompt" {
    const gpa = std.testing.allocator;
    var loader = SkillLoader.init(gpa);
    const skills = [_]Skill{
        .{ .name = "skill-a", .description = "desc a", .path = "/path/a" },
        .{ .name = "skill-b", .description = "desc b", .path = "/path/b" },
    };
    const prompt = try loader.buildSystemPrompt(&skills);
    defer gpa.free(prompt);

    try std.testing.expect(std.mem.indexOf(u8, prompt, "<available_skills>") != null);
    try std.testing.expect(std.mem.indexOf(u8, prompt, "skill-a") != null);
    try std.testing.expect(std.mem.indexOf(u8, prompt, "skill-b") != null);
}

const ActionTestCtx = struct {
    last_action: ?Action = null,
    last_plugin_id: []const u8 = "",
};

fn actionTestCallback(ctx: ?*anyopaque, plugin_id: []const u8, action: Action) void {
    const tc: *ActionTestCtx = @ptrCast(@alignCast(ctx.?));
    tc.last_action = action;
    tc.last_plugin_id = plugin_id;
}

test "registry dispatches actions" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();
    var registry = Registry.init(gpa, io);
    defer registry.deinit();

    var test_ctx = ActionTestCtx{};
    registry.setActionHandler(&test_ctx, actionTestCallback);

    registry.dispatchAction("plugin1", .{ .set_model = "gpt-4o" });
    try std.testing.expectEqualStrings("plugin1", test_ctx.last_plugin_id);
    switch (test_ctx.last_action.?) {
        .set_model => |m| try std.testing.expectEqualStrings("gpt-4o", m),
        else => return error.WrongAction,
    }

    registry.dispatchAction("plugin2", .{ .set_session_name = "test-session" });
    switch (test_ctx.last_action.?) {
        .set_session_name => |n| try std.testing.expectEqualStrings("test-session", n),
        else => return error.WrongAction,
    }
}
