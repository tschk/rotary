const std = @import("std");

const log = std.log.scoped(.acp);

pub const AgentId = []const u8;

pub const AcpMessage = struct {
    jsonrpc: []const u8 = "2.0",
    id: ?u64 = null,
    method: ?[]const u8 = null,
    params: ?[]const u8 = null,
    result: ?[]const u8 = null,
    @"error": ?AcpError = null,
};

pub const AcpError = struct {
    code: i32,
    message: []const u8,
};

fn readFrame(reader: *std.Io.Reader, max_len: usize) ![]const u8 {
    var content_length: ?usize = null;
    while (true) {
        const line = try reader.takeDelimiter('\n') orelse return error.InvalidFrame;
        const trimmed = std.mem.trim(u8, line, " \r\n");
        if (trimmed.len == 0) break;
        const colon = std.mem.indexOfScalar(u8, trimmed, ':') orelse return error.InvalidFrame;
        if (!std.ascii.eqlIgnoreCase(trimmed[0..colon], "Content-Length")) return error.InvalidFrame;
        if (content_length != null) return error.InvalidFrame;
        const len = try std.fmt.parseInt(usize, std.mem.trim(u8, trimmed[colon + 1 ..], " "), 10);
        if (len > max_len) return error.FrameTooLarge;
        content_length = len;
    }
    return reader.take(content_length orelse return error.InvalidFrame);
}

pub const AgentProcess = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    id: []const u8,
    command: []const u8,
    args: []const []const u8,
    process: ?std.process.Child = null,
    next_id: u64 = 1,
    capabilities_sent: bool = false,

    pub fn init(allocator: std.mem.Allocator, io: std.Io, id: []const u8, command: []const u8, args: []const []const u8) AgentProcess {
        return .{
            .allocator = allocator,
            .io = io,
            .id = id,
            .command = command,
            .args = args,
        };
    }

    pub fn deinit(self: *AgentProcess) void {
        self.kill();
    }

    pub fn start(self: *AgentProcess) !void {
        var argv: std.ArrayList([]const u8) = .empty;
        defer argv.deinit(self.allocator);
        try argv.append(self.allocator, self.command);
        for (self.args) |arg| {
            try argv.append(self.allocator, arg);
        }
        self.process = try std.process.spawn(self.io, .{
            .argv = argv.items,
            .stdin = .pipe,
            .stdout = .pipe,
            .stderr = .inherit,
        });
        log.info("started ACP agent {s}: {s}", .{ self.id, self.command });
    }

    pub fn kill(self: *AgentProcess) void {
        if (self.process) |*p| {
            p.kill(self.io);

            self.process = null;
        }
    }

    pub fn nextId(self: *AgentProcess) u64 {
        const id = self.next_id;
        self.next_id += 1;
        return id;
    }

    pub fn sendRequest(self: *AgentProcess, method: []const u8, params_json: []const u8) !u64 {
        if (self.process == null) return error.AgentNotRunning;
        const file = self.process.?.stdin orelse return error.AgentNotRunning;

        const id = self.nextId();

        var json_buf: std.Io.Writer.Allocating = .init(self.allocator);
        const jw = &json_buf.writer;
        defer json_buf.deinit();

        try jw.writeAll("{\"jsonrpc\":\"2.0\",\"id\":");
        try jw.print("{d}", .{id});
        try jw.writeAll(",\"method\":\"");
        try jw.writeAll(method);
        try jw.writeAll("\",\"params\":");
        try jw.writeAll(params_json);
        try jw.writeAll("}");

        const content = json_buf.written();
        var buf: [4096]u8 = undefined;
        var file_writer: std.Io.File.Writer = .init(file, self.io, &buf);
        const writer = &file_writer.interface;
        try writer.print("Content-Length: {d}\r\n\r\n", .{content.len});
        try writer.writeAll(content);
        try writer.flush();

        return id;
    }

    pub fn sendNotification(self: *AgentProcess, method: []const u8, params_json: []const u8) !void {
        if (self.process == null) return error.AgentNotRunning;
        const file = self.process.?.stdin orelse return error.AgentNotRunning;

        var json_buf: std.Io.Writer.Allocating = .init(self.allocator);
        const jw = &json_buf.writer;
        defer json_buf.deinit();

        try jw.writeAll("{\"jsonrpc\":\"2.0\",\"method\":\"");
        try jw.writeAll(method);
        try jw.writeAll("\",\"params\":");
        try jw.writeAll(params_json);
        try jw.writeAll("}");

        const content = json_buf.written();
        var buf: [4096]u8 = undefined;
        var file_writer: std.Io.File.Writer = .init(file, self.io, &buf);
        const writer = &file_writer.interface;
        try writer.print("Content-Length: {d}\r\n\r\n", .{content.len});
        try writer.writeAll(content);
        try writer.flush();
    }

    pub fn initialize(self: *AgentProcess, client_capabilities_json: []const u8) !void {
        const request_id = try self.sendRequest("initialize", client_capabilities_json);
        var read_buf: [65536]u8 = undefined;
        _ = try self.waitForResponse(request_id, &read_buf);
        try self.sendNotification("initialized", "{}");
        self.capabilities_sent = true;
        log.info("ACP agent {s} initialized", .{self.id});
    }

    pub fn prompt(self: *AgentProcess, message: []const u8) !u64 {
        var params_buf: std.Io.Writer.Allocating = .init(self.allocator);
        const pw = &params_buf.writer;
        defer params_buf.deinit();

        try pw.writeAll("{\"prompt\":");
        try std.json.Stringify.encodeJsonString(message, .{}, pw);
        try pw.writeAll("}");

        return try self.sendRequest("prompt", params_buf.written());
    }

    pub fn waitForResponse(self: *AgentProcess, expected_id: u64, read_buf: []u8) ![]const u8 {
        if (self.process == null) return error.AgentNotRunning;
        const file = self.process.?.stdout orelse return error.AgentNotRunning;
        var file_reader = std.Io.File.reader(file, self.io, read_buf);
        const reader = &file_reader.interface;

        const body = try readFrame(reader, read_buf.len);
        const parsed = try std.json.parseFromSlice(std.json.Value, self.allocator, body, .{});
        defer parsed.deinit();
        if (parsed.value != .object) return error.InvalidResponse;
        const id_value = parsed.value.object.get("id") orelse return error.InvalidResponse;
        const id = switch (id_value) {
            .integer => |value| std.math.cast(u64, value) orelse return error.InvalidResponse,
            else => return error.InvalidResponse,
        };
        if (id != expected_id) return error.ResponseIdMismatch;
        if (parsed.value.object.get("error") != null) return error.RemoteError;
        if (parsed.value.object.get("result") == null) return error.InvalidResponse;
        return body;
    }

    /// Send a prompt and wait for the response line.
    /// Returns the raw JSON-RPC response (Content-Length header parsed).
    pub fn promptAndWait(self: *AgentProcess, message: []const u8, read_buf: []u8) ![]const u8 {
        const request_id = try self.prompt(message);
        return self.waitForResponse(request_id, read_buf);
    }
};

pub const Host = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    agents: std.StringHashMap(*AgentProcess),

    pub fn init(allocator: std.mem.Allocator, io: std.Io) Host {
        return .{
            .allocator = allocator,
            .io = io,
            .agents = std.StringHashMap(*AgentProcess).init(allocator),
        };
    }

    pub fn deinit(self: *Host) void {
        var iter = self.agents.iterator();
        while (iter.next()) |entry| {
            entry.value_ptr.*.deinit();
            self.allocator.destroy(entry.value_ptr.*);
        }
        self.agents.deinit();
    }

    pub fn spawn(self: *Host, id: []const u8, command: []const u8, args: []const []const u8) !*AgentProcess {
        const agent_proc = try self.allocator.create(AgentProcess);
        agent_proc.* = AgentProcess.init(self.allocator, self.io, id, command, args);
        try agent_proc.start();
        try self.agents.put(id, agent_proc);
        log.info("host spawned agent: {s}", .{id});
        return agent_proc;
    }

    pub fn get(self: *Host, id: []const u8) ?*AgentProcess {
        return self.agents.get(id);
    }

    pub fn kill(self: *Host, id: []const u8) void {
        if (self.agents.fetchRemove(id)) |kv| {
            kv.value.deinit();
            self.allocator.destroy(kv.value);
        }
    }

    pub fn killAll(self: *Host) void {
        var iter = self.agents.iterator();
        while (iter.next()) |entry| {
            entry.value_ptr.*.kill();
        }
    }
};

test "host spawn and get agent" {
    const gpa = std.testing.allocator;
    const io = std.testing.io;
    var host = Host.init(gpa, io);
    defer host.deinit();

    _ = try host.spawn("test-agent", "echo", &.{"hello"});
    try std.testing.expect(host.get("test-agent") != null);
}

test "agent process next id increments" {
    const gpa = std.testing.allocator;
    const io = std.testing.io;
    var agent_proc = AgentProcess.init(gpa, io, "test", "echo", &.{});
    defer agent_proc.deinit();

    try std.testing.expectEqual(@as(u64, 1), agent_proc.nextId());
    try std.testing.expectEqual(@as(u64, 2), agent_proc.nextId());
}

test "agent process initializes and prompts a framed child" {
    const gpa = std.testing.allocator;
    const io = std.testing.io;
    const script =
        "read h; read b; n=${h#Content-Length: }; dd bs=1 count=$n of=/dev/null 2>/dev/null; " ++
        "printf 'Content-Length: 36\\r\\n\\r\\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'; " ++
        "read h; read b; n=${h#Content-Length: }; dd bs=1 count=$n of=/dev/null 2>/dev/null; " ++
        "read h; read b; n=${h#Content-Length: }; dd bs=1 count=$n of=/dev/null 2>/dev/null; " ++
        "printf 'Content-Length: 48\\r\\n\\r\\n{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"reply\":\"ok\"}}'";
    var agent_proc = AgentProcess.init(gpa, io, "fake", "/bin/sh", &.{ "-c", script });
    defer agent_proc.deinit();
    try agent_proc.start();
    try agent_proc.initialize("{\"capabilities\":{}}");
    var read_buf: [1024]u8 = undefined;
    const response = try agent_proc.promptAndWait("hello", &read_buf);
    try std.testing.expect(std.mem.indexOf(u8, response, "\"reply\":\"ok\"") != null);
}

test "framed response rejects malformed headers" {
    var reader = std.Io.Reader.fixed("Length: 2\r\n\r\n{}");
    try std.testing.expectError(error.InvalidFrame, readFrame(&reader, 16));
}
