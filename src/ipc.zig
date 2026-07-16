const std = @import("std");
const agent = @import("agent.zig");
const provider = @import("provider.zig");
const plugin = @import("plugin.zig");
const session = @import("session.zig");
const acp = @import("acp.zig");

const log = std.log.scoped(.ipc);

var shutdown_requested: std.atomic.Value(bool) = .init(false);

fn shutdownHandler(sig: std.posix.SIG) callconv(.c) void {
    _ = sig;
    shutdown_requested.store(true, .release);
}

fn installSignalHandlers() void {
    if (@import("builtin").os.tag == .windows) return;
    const act: std.posix.Sigaction = .{
        .handler = .{ .handler = shutdownHandler },
        .mask = std.posix.sigemptyset(),
        .flags = 0,
    };
    std.posix.sigaction(.INT, &act, null);
    std.posix.sigaction(.TERM, &act, null);
}

const ShutdownWatcher = struct {
    socket_path: []const u8,

    fn run(self: ShutdownWatcher) void {
        if (@import("builtin").os.tag == .windows) return;
        const ts: std.posix.timespec = .{ .sec = 0, .nsec = 100 * std.time.ns_per_ms };
        while (!shutdown_requested.load(.acquire)) {
            _ = std.posix.system.nanosleep(&ts, null);
        }
        log.info("shutdown detected, breaking accept", .{});
        const io = std.Io.Threaded.global_single_threaded.io();
        const unix_addr = std.Io.net.UnixAddress.init(self.socket_path) catch return;
        const stream = std.Io.net.UnixAddress.connect(&unix_addr, io) catch return;
        stream.close(io);
    }
};

pub const RpcError = error{
    MethodNotFound,
    InvalidParams,
    InternalError,
};

pub const Server = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    socket_path: []const u8,
    agent_instance: ?*agent.Agent = null,
    tool_registry: ?*agent.ToolRegistry = null,
    plugin_registry: ?*plugin.Registry = null,
    session_store: ?*session.Store = null,
    acp_host: ?*acp.Host = null,
    current_session: ?session.Session = null,
    session_counter: u64 = 0,
    arena: std.heap.ArenaAllocator,
    event_subscriber_ctx: EventSubscriberCtx,

    const EventSubscriberCtx = struct {
        server: *Server,
        current_writer: ?*std.Io.Writer = null,
    };

    pub fn init(allocator: std.mem.Allocator, io: std.Io, socket_path: []const u8) Server {
        return .{
            .allocator = allocator,
            .io = io,
            .socket_path = socket_path,
            .arena = std.heap.ArenaAllocator.init(allocator),
            .event_subscriber_ctx = undefined,
        };
    }

    pub fn deinit(self: *Server) void {
        if (self.current_session) |*cs| cs.deinit();
        self.arena.deinit();
    }

    pub fn attachAgent(self: *Server, a: *agent.Agent) void {
        self.agent_instance = a;
        self.event_subscriber_ctx = .{ .server = self, .current_writer = null };
        a.subscribe(&self.event_subscriber_ctx, eventCallback) catch {};
    }

    pub fn attachTools(self: *Server, tr: *agent.ToolRegistry) void {
        self.tool_registry = tr;
    }

    pub fn attachPlugins(self: *Server, pr: *plugin.Registry) void {
        self.plugin_registry = pr;
    }

    pub fn attachSessionStore(self: *Server, ss: *session.Store) void {
        self.session_store = ss;
    }

    pub fn attachAcpHost(self: *Server, host: *acp.Host) void {
        self.acp_host = host;
    }

    fn sessionCounter(self: *Server) u64 {
        self.session_counter += 1;
        return self.session_counter;
    }

    fn eventCallback(ctx: ?*anyopaque, event: agent.Event) void {
        const esc: *EventSubscriberCtx = @ptrCast(@alignCast(ctx.?));
        if (esc.current_writer) |w| {
            pushEvent(w, event) catch {};
        }
    }

    pub fn run(self: *Server) !void {
        _ = self.arena.reset(.retain_capacity);
        const arena = self.arena.allocator();

        std.Io.Dir.deleteFileAbsolute(self.io, self.socket_path) catch {};

        const unix_addr = try std.Io.net.UnixAddress.init(self.socket_path);
        var server = try std.Io.net.UnixAddress.listen(&unix_addr, self.io, .{});
        defer server.deinit(self.io);

        installSignalHandlers();
        log.info("IPC server listening on {s}", .{self.socket_path});
        log.info("press Ctrl+C to shut down", .{});

        // Spawn watcher thread to break accept on shutdown
        if (@import("builtin").os.tag != .windows) {
            const watcher_thread = std.Thread.spawn(.{}, ShutdownWatcher.run, .{ShutdownWatcher{ .socket_path = self.socket_path }}) catch null;
            if (watcher_thread) |t| t.detach();
        }

        while (!shutdown_requested.load(.acquire)) {
            const stream = server.accept(self.io) catch |err| {
                if (err == error.SystemResources) continue;
                if (err == error.SocketNotListening) break;
                if (shutdown_requested.load(.acquire)) break;
                log.err("accept failed: {}", .{err});
                continue;
            };

            if (shutdown_requested.load(.acquire)) {
                stream.close(self.io);
                break;
            }

            self.handleConnection(arena, stream) catch |err| {
                log.err("connection error: {}", .{err});
            };
            stream.close(self.io);
            _ = self.arena.reset(.retain_capacity);
        }

        log.info("shutting down IPC server", .{});
        std.Io.Dir.deleteFileAbsolute(self.io, self.socket_path) catch {};
    }

    fn handleConnection(self: *Server, arena: std.mem.Allocator, stream: std.Io.net.Stream) !void {
        var read_buf: [16384]u8 = undefined;
        var reader = std.Io.net.Stream.Reader.init(stream, self.io, &read_buf);
        const r = &reader.interface;

        var write_buf: [16384]u8 = undefined;
        var writer = std.Io.net.Stream.Writer.init(stream, self.io, &write_buf);
        const w = &writer.interface;

        while (true) {
            const line = r.takeDelimiter('\n') catch |err| {
                if (err == error.EndOfStream) return;
                return err;
            };
            if (line == null) return;

            self.event_subscriber_ctx.current_writer = w;
            const response = try self.handleRequest(arena, line.?);
            try w.print("{s}\n", .{response});
            try w.flush();
            self.event_subscriber_ctx.current_writer = null;
        }
    }

    fn handleRequest(self: *Server, arena: std.mem.Allocator, line: []const u8) ![]const u8 {
        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;

        const parsed = std.json.parseFromSlice(Request, arena, line, .{
            .ignore_unknown_fields = true,
        }) catch {
            try writeError(out, null, -32700, "Parse error");
            return buf.written();
        };

        const result = self.dispatch(arena, parsed.value.method, parsed.value.params) catch |err| {
            const msg = @errorName(err);
            try writeError(out, parsed.value.id, -32603, msg);
            return buf.written();
        };

        try writeResult(out, parsed.value.id, result);
        return buf.written();
    }

    fn dispatch(self: *Server, arena: std.mem.Allocator, method: []const u8, params: ?std.json.Value) ![]const u8 {
        if (std.mem.eql(u8, method, "state")) return self.getState(arena);
        if (std.mem.eql(u8, method, "prompt")) return self.sendPrompt(arena, params);
        if (std.mem.eql(u8, method, "set_model")) return self.setModel(arena, params);
        if (std.mem.eql(u8, method, "tools")) return self.getTools(arena);
        if (std.mem.eql(u8, method, "plugins")) return self.getPlugins(arena);
        if (std.mem.eql(u8, method, "messages")) return self.getMessages(arena);
        if (std.mem.eql(u8, method, "call_tool")) return self.callTool(arena, params);
        if (std.mem.eql(u8, method, "ping")) return self.ping(arena);
        if (std.mem.eql(u8, method, "session_list")) return self.sessionList(arena);
        if (std.mem.eql(u8, method, "session_create")) return self.sessionCreate(arena, params);
        if (std.mem.eql(u8, method, "session_load")) return self.sessionLoad(arena, params);
        if (std.mem.eql(u8, method, "session_save")) return self.sessionSave(arena);
        if (std.mem.eql(u8, method, "session_clear")) return self.sessionClear(arena);
        if (std.mem.eql(u8, method, "session_fork")) return self.sessionFork(arena, params);
        if (std.mem.eql(u8, method, "session_merge")) return self.sessionMerge(arena, params);
        if (std.mem.eql(u8, method, "session_tree")) return self.sessionTree(arena, params);
        return error.MethodNotFound;
    }

    fn ping(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        _ = self;
        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"pong\":true}}", .{});
        return buf.written();
    }

    fn getState(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;
        try out.print("{{\"model\":\"{s}\",\"messages\":{d},\"tools\":{d}", .{
            if (self.agent_instance) |a| a.model else "none",
            if (self.agent_instance) |a| a.messages.items.len else 0,
            if (self.tool_registry) |tr| tr.tools.count() else 0,
        });
        if (self.plugin_registry) |pr| {
            try out.print(",\"plugins\":{d}", .{pr.plugins.items.len});
        } else {
            try out.print(",\"plugins\":0", .{});
        }
        try out.print("}}", .{});
        return buf.written();
    }

    fn sendPrompt(self: *Server, arena: std.mem.Allocator, params: ?std.json.Value) ![]const u8 {
        const text = blk: {
            const p = params orelse return error.InvalidParams;
            switch (p) {
                .object => |obj| {
                    const text_val = obj.get("text") orelse return error.InvalidParams;
                    switch (text_val) {
                        .string => |s| break :blk s,
                        else => return error.InvalidParams,
                    }
                },
                else => return error.InvalidParams,
            }
        };

        if (self.agent_instance) |a| {
            try a.prompt(text);
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"ok\":true}}", .{});
        return buf.written();
    }

    fn setModel(self: *Server, arena: std.mem.Allocator, params: ?std.json.Value) ![]const u8 {
        const model = blk: {
            const p = params orelse return error.InvalidParams;
            switch (p) {
                .object => |obj| {
                    const val = obj.get("model") orelse return error.InvalidParams;
                    switch (val) {
                        .string => |s| break :blk s,
                        else => return error.InvalidParams,
                    }
                },
                else => return error.InvalidParams,
            }
        };

        if (self.agent_instance) |a| {
            a.setModel(model);
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"ok\":true}}", .{});
        return buf.written();
    }

    fn getTools(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;
        try out.print("[", .{});
        if (self.tool_registry) |tr| {
            var it = tr.tools.iterator();
            var first = true;
            while (it.next()) |entry| {
                if (!first) try out.print(",", .{});
                first = false;
                try out.print("{{\"name\":\"{s}\",\"description\":\"{s}\"}}", .{
                    entry.key_ptr.*,
                    entry.value_ptr.description,
                });
            }
        }
        try out.print("]", .{});
        return buf.written();
    }

    fn getPlugins(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;
        try out.print("[", .{});
        if (self.plugin_registry) |pr| {
            for (pr.plugins.items, 0..) |p, i| {
                if (i > 0) try out.print(",", .{});
                try out.print("{{\"id\":\"{s}\",\"name\":\"{s}\",\"ready\":{},\"tools\":{d},\"commands\":{d}}}", .{
                    p.id, p.name, p.ready, p.registered_tools.items.len, p.registered_commands.items.len,
                });
            }
        }
        try out.print("]", .{});
        return buf.written();
    }

    fn getMessages(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;
        try out.print("[", .{});
        if (self.agent_instance) |a| {
            for (a.messages.items, 0..) |msg, i| {
                if (i > 0) try out.print(",", .{});
                const role_str = switch (msg.role) {
                    .user => "user",
                    .assistant => "assistant",
                    .system => "system",
                    .tool => "tool",
                };
                try out.print("{{\"role\":\"{s}\",\"content\":", .{role_str});
                try std.json.Stringify.value(msg.content, .{}, out);
                try out.print("}}", .{});
            }
        }
        try out.print("]", .{});
        return buf.written();
    }

    fn callTool(self: *Server, arena: std.mem.Allocator, params: ?std.json.Value) ![]const u8 {
        const p = params orelse return error.InvalidParams;
        const obj = switch (p) {
            .object => |o| o,
            else => return error.InvalidParams,
        };
        const name = switch (obj.get("name") orelse return error.InvalidParams) {
            .string => |s| s,
            else => return error.InvalidParams,
        };
        const arguments = blk: {
            const args_val = obj.get("arguments") orelse break :blk "{}";
            switch (args_val) {
                .string => |s| break :blk s,
                else => break :blk "{}",
            }
        };

        if (self.tool_registry) |tr| {
            if (tr.get(name)) |tool| {
                const result = try tool.execute(tool.ctx, self.allocator, arguments);
                defer self.allocator.free(result.content);

                var buf: std.Io.Writer.Allocating = .init(arena);
                const out = &buf.writer;
                try out.print("{{\"content\":", .{});
                try std.json.Stringify.value(result.content, .{}, out);
                try out.print(",\"is_error\":{}}}", .{result.is_error});
                return buf.written();
            }
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"error\":\"tool not found\"}}", .{});
        return buf.written();
    }

    fn sessionList(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;
        try out.print("[", .{});

        if (self.session_store) |ss| {
            var dir = std.Io.Dir.cwd().openDir(self.io, ss.base_dir, .{ .iterate = true }) catch {
                try out.print("]", .{});
                return buf.written();
            };
            defer dir.close(self.io);

            var iter = dir.iterate();
            var first = true;
            while (try iter.next(self.io)) |entry| {
                if (entry.kind != .file) continue;
                if (!std.mem.endsWith(u8, entry.name, ".jsonl")) continue;
                const id = entry.name[0 .. entry.name.len - 6];
                if (!first) try out.print(",", .{});
                first = false;
                try out.print("{{\"id\":", .{});
                try std.json.Stringify.value(id, .{}, out);
                try out.print("}}", .{});
            }
        }

        try out.print("]", .{});
        return buf.written();
    }

    fn sessionCreate(self: *Server, arena: std.mem.Allocator, params: ?std.json.Value) ![]const u8 {
        const name = blk: {
            const p = params orelse break :blk "default";
            switch (p) {
                .object => |obj| {
                    const val = obj.get("name") orelse break :blk "default";
                    switch (val) {
                        .string => |s| break :blk s,
                        else => break :blk "default",
                    }
                },
                else => break :blk "default",
            }
        };

        // Generate session ID from timestamp
        var id_buf: [32]u8 = undefined;
        const id = try std.fmt.bufPrint(&id_buf, "s-{d}", .{self.sessionCounter()});

        // Clear current session if any
        if (self.current_session) |*cs| cs.deinit();
        self.current_session = try session.Session.init(self.allocator, id, name);

        // Clear agent messages
        if (self.agent_instance) |a| {
            a.clearMessages();
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"id\":", .{});
        try std.json.Stringify.value(id, .{}, &buf.writer);
        try buf.writer.print(",\"name\":", .{});
        try std.json.Stringify.value(name, .{}, &buf.writer);
        try buf.writer.print("}}", .{});
        return buf.written();
    }

    fn sessionLoad(self: *Server, arena: std.mem.Allocator, params: ?std.json.Value) ![]const u8 {
        const id = blk: {
            const p = params orelse return error.InvalidParams;
            switch (p) {
                .object => |obj| {
                    const val = obj.get("id") orelse return error.InvalidParams;
                    switch (val) {
                        .string => |s| break :blk s,
                        else => return error.InvalidParams,
                    }
                },
                else => return error.InvalidParams,
            }
        };

        const ss = self.session_store orelse return error.InternalError;

        // Clear current session
        if (self.current_session) |*cs| cs.deinit();
        self.current_session = try ss.load(id);

        // Load messages into agent
        if (self.agent_instance) |a| {
            a.clearMessages();
            for (self.current_session.?.entries.items) |entry| {
                try a.messages.append(self.allocator, .{
                    .role = entry.role,
                    .content = try self.allocator.dupe(u8, entry.content),
                });
            }
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"id\":", .{});
        try std.json.Stringify.value(id, .{}, &buf.writer);
        try buf.writer.print(",\"messages\":{d}}}", .{self.current_session.?.messageCount()});
        return buf.written();
    }

    fn sessionSave(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        const ss = self.session_store orelse return error.InternalError;

        // If no current session, create one
        if (self.current_session == null) {
            var id_buf: [32]u8 = undefined;
            const id = try std.fmt.bufPrint(&id_buf, "s-{d}", .{self.sessionCounter()});
            self.current_session = try session.Session.init(self.allocator, id, "auto");
        }

        // Sync agent messages to session
        if (self.agent_instance) |a| {
            // Rebuild session entries from agent messages
            if (self.current_session) |*cs| {
                for (cs.entries.items) |entry| {
                    self.allocator.free(entry.session_id);
                    self.allocator.free(entry.content);
                    if (entry.tool_call_id) |tcid| self.allocator.free(tcid);
                }
                cs.entries.clearRetainingCapacity();
                for (a.messages.items) |msg| {
                    _ = try cs.append(msg.role, msg.content);
                }
            }
        }

        if (self.current_session) |*cs| {
            try ss.save(cs);
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"ok\":true}}", .{});
        return buf.written();
    }

    fn sessionClear(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        if (self.current_session) |*cs| {
            cs.deinit();
            self.current_session = null;
        }
        if (self.agent_instance) |a| {
            a.clearMessages();
        }
        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"ok\":true}}", .{});
        return buf.written();
    }

    fn sessionFork(self: *Server, arena: std.mem.Allocator, params: ?std.json.Value) ![]const u8 {
        if (self.current_session == null) {
            // Create a fresh session if none exists
            var id_buf: [32]u8 = undefined;
            const id = try std.fmt.bufPrint(&id_buf, "s-{d}", .{self.sessionCounter()});
            self.current_session = try session.Session.init(self.allocator, id, "forked");
            var buf: std.Io.Writer.Allocating = .init(arena);
            try buf.writer.print("{{\"id\":", .{});
            try std.json.Stringify.value(id, .{}, &buf.writer);
            try buf.writer.print(",\"messages\":0}}", .{});
            return buf.written();
        }

        const from_entry = blk: {
            const p = params orelse break :blk self.current_session.?.entries.items.len;
            switch (p) {
                .object => |obj| {
                    if (obj.get("from_entry")) |v| {
                        switch (v) {
                            .integer => |n| break :blk @as(u64, @intCast(n)),
                            else => break :blk self.current_session.?.entries.items.len,
                        }
                    }
                    break :blk self.current_session.?.entries.items.len;
                },
                else => break :blk self.current_session.?.entries.items.len,
            }
        };

        var id_buf: [32]u8 = undefined;
        const fork_id = try std.fmt.bufPrint(&id_buf, "fork-{d}", .{self.sessionCounter()});

        var forked = try self.current_session.?.fork(self.allocator, fork_id, from_entry);
        errdefer forked.deinit();

        // Save the fork
        if (self.session_store) |ss| {
            try ss.save(&forked);
        }

        // Switch session to fork
        if (self.current_session) |*cs| cs.deinit();
        self.current_session = forked;

        // Reload agent messages
        if (self.agent_instance) |a| {
            a.clearMessages();
            for (self.current_session.?.entries.items) |entry| {
                try a.messages.append(self.allocator, .{
                    .role = entry.role,
                    .content = try self.allocator.dupe(u8, entry.content),
                });
            }
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"id\":", .{});
        try std.json.Stringify.value(fork_id, .{}, &buf.writer);
        try buf.writer.print(",\"messages\":{d}}}", .{self.current_session.?.messageCount()});
        return buf.written();
    }

    fn sessionMerge(self: *Server, arena: std.mem.Allocator, params: ?std.json.Value) ![]const u8 {
        const src_id = blk: {
            const p = params orelse return error.InvalidParams;
            switch (p) {
                .object => |obj| {
                    const val = obj.get("id") orelse return error.InvalidParams;
                    switch (val) {
                        .string => |s| break :blk s,
                        else => return error.InvalidParams,
                    }
                },
                else => return error.InvalidParams,
            }
        };

        const ss = self.session_store orelse return error.InternalError;

        // Load the source session
        var src_session = try ss.load(src_id);
        defer src_session.deinit();

        // Merge into current session
        if (self.current_session == null) {
            // No current session — just switch to the loaded one
            self.current_session = src_session;
            // Can't defer deinit since we moved it
            if (self.agent_instance) |a| {
                a.clearMessages();
                for (self.current_session.?.entries.items) |entry| {
                    try a.messages.append(self.allocator, .{
                        .role = entry.role,
                        .content = try self.allocator.dupe(u8, entry.content),
                    });
                }
            }

            var buf: std.Io.Writer.Allocating = .init(arena);
            try buf.writer.print("{{\"merged\":{d},\"total\":{d}}}", .{
                self.current_session.?.messageCount(),
                self.current_session.?.messageCount(),
            });
            return buf.written();
        }

        const count = try self.current_session.?.merge(&src_session);

        // Reload agent messages
        if (self.agent_instance) |a| {
            a.clearMessages();
            for (self.current_session.?.entries.items) |entry| {
                try a.messages.append(self.allocator, .{
                    .role = entry.role,
                    .content = try self.allocator.dupe(u8, entry.content),
                });
            }
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"merged\":{d},\"total\":{d}}}", .{
            count,
            self.current_session.?.messageCount(),
        });
        return buf.written();
    }

    fn sessionTree(self: *Server, arena: std.mem.Allocator, params: ?std.json.Value) ![]const u8 {
        // Determine which session to inspect
        var target_session: ?*const session.Session = null;
        if (self.current_session) |*cs| {
            target_session = cs;
        }

        // If params.session_id is provided, load that session instead
        if (params) |p| {
            if (p == .object) {
                if (p.object.get("session_id")) |sid_v| {
                    if (sid_v == .string) {
                        if (self.session_store) |ss| {
                            const loaded = try ss.load(sid_v.string);
                            // Allocate on arena so it lives long enough
                            const arena_alloc = arena;
                            const heap_session = try arena_alloc.create(session.Session);
                            heap_session.* = loaded;
                            target_session = heap_session;
                        }
                    }
                }
            }
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;

        if (target_session) |s| {
            try out.print("[", .{});
            for (s.entries.items, 0..) |entry, i| {
                if (i > 0) try out.print(",", .{});
                const role_str = switch (entry.role) {
                    .user => "user",
                    .assistant => "assistant",
                    .system => "system",
                    .tool => "tool",
                };
                // Truncate content for display
                const preview_len = @min(entry.content.len, @as(usize, 80));
                const content_preview = entry.content[0..preview_len];

                try out.print("{{\"id\":{d},\"parent_id\":", .{entry.id});
                if (entry.parent_id) |pid| {
                    try out.print("{d}", .{pid});
                } else {
                    try out.print("null", .{});
                }
                try out.print(",\"role\":\"{s}\",\"content\":", .{role_str});
                try std.json.Stringify.value(content_preview, .{}, out);
                try out.print(",\"depth\":0", .{}); // Depth computed client-side
                try out.print("}}", .{});
            }
            try out.print("]", .{});
        } else {
            try out.print("[]", .{});
        }
        return buf.written();
    }

    fn writeResult(out: *std.Io.Writer, id: ?std.json.Value, result: []const u8) !void {
        try out.print("{{\"jsonrpc\":\"2.0\",\"id\":", .{});
        if (id) |v| {
            try std.json.Stringify.value(v, .{}, out);
        } else {
            try out.print("null", .{});
        }
        try out.print(",\"result\":{s}}}", .{result});
    }

    fn writeError(out: *std.Io.Writer, id: ?std.json.Value, code: i32, message: []const u8) !void {
        try out.print("{{\"jsonrpc\":\"2.0\",\"id\":", .{});
        if (id) |v| {
            try std.json.Stringify.value(v, .{}, out);
        } else {
            try out.print("null", .{});
        }
        try out.print(",\"error\":{{\"code\":{d},\"message\":\"{s}\"}}}}", .{ code, message });
    }
};

fn pushEvent(w: *std.Io.Writer, event: agent.Event) !void {
    var buf: std.Io.Writer.Allocating = .init(std.heap.page_allocator);
    defer buf.deinit();
    const out = &buf.writer;

    try out.print("{{\"jsonrpc\":\"2.0\",\"method\":\"event\",\"params\":{{\"type\":\"{s}\"", .{eventTypeName(event)});

    switch (event) {
        .message_start => |msg| {
            try out.print(",\"role\":\"{s}\"", .{roleName(msg.role)});
        },
        .message_update => |text| {
            try out.print(",\"delta\":", .{});
            try std.json.Stringify.value(text, .{}, out);
        },
        .message_end => |msg| {
            try out.print(",\"role\":\"{s}\",\"content\":", .{roleName(msg.role)});
            try std.json.Stringify.value(msg.content, .{}, out);
        },
        .tool_call => |tc| {
            try out.print(",\"tool_name\":\"{s}\",\"arguments\":", .{tc.name});
            try std.json.Stringify.value(tc.arguments, .{}, out);
        },
        .tool_execution_start => |tc| {
            try out.print(",\"tool_name\":\"{s}\"", .{tc.name});
        },
        .tool_execution_update => |text| {
            try out.print(",\"delta\":", .{});
            try std.json.Stringify.value(text, .{}, out);
        },
        .tool_execution_end => |tr| {
            try out.print(",\"is_error\":{}", .{tr.is_error});
        },
        .tool_result => |tr| {
            try out.print(",\"is_error\":{}", .{tr.is_error});
        },
        .before_agent_start => |bas| {
            try out.print(",\"message_count\":{d}", .{bas.messages.len});
        },
        else => {},
    }

    try out.print("}}}}", .{});
    try w.print("{s}\n", .{buf.written()});
    try w.flush();
}

fn eventTypeName(event: agent.Event) []const u8 {
    return switch (event) {
        .before_agent_start => "before_agent_start",
        .agent_start => "agent_start",
        .turn_start => "turn_start",
        .message_start => "message_start",
        .message_update => "message_update",
        .message_end => "message_end",
        .tool_call => "tool_call",
        .tool_execution_start => "tool_execution_start",
        .tool_execution_update => "tool_execution_update",
        .tool_execution_end => "tool_execution_end",
        .tool_result => "tool_result",
        .turn_end => "turn_end",
        .agent_end => "agent_end",
    };
}

fn roleName(role: provider.Role) []const u8 {
    return switch (role) {
        .user => "user",
        .assistant => "assistant",
        .system => "system",
        .tool => "tool",
    };
}

const Request = struct {
    jsonrpc: []const u8 = "2.0",
    id: ?std.json.Value = null,
    method: []const u8,
    params: ?std.json.Value = null,
};

test "ipc server init/deinit" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();
    var server = Server.init(gpa, io, "/tmp/rotary-test.sock");
    defer server.deinit();
    try std.testing.expectEqualStrings("/tmp/rotary-test.sock", server.socket_path);
}
