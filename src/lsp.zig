const std = @import("std");

const log = std.log.scoped(.lsp);

pub const LanguageId = []const u8;

pub const Diagnostic = struct {
    uri: []const u8,
    line: u32,
    character: u32,
    severity: Severity,
    message: []const u8,
    source: ?[]const u8 = null,

    pub const Severity = enum(u8) {
        err = 1,
        warn = 2,
        info = 3,
        hint = 4,
    };
};

pub const Location = struct {
    uri: []const u8,
    start_line: u32,
    start_character: u32,
    end_line: u32,
    end_character: u32,
};

pub const Hover = struct {
    contents: []const u8,

    pub fn deinit(self: Hover, allocator: std.mem.Allocator) void {
        allocator.free(self.contents);
    }
};

pub const ServerConfig = struct {
    language: []const u8,
    command: []const u8,
    args: []const []const u8 = &.{},
};

pub const default_servers = [_]ServerConfig{
    .{ .language = "zig", .command = "zls" },
    .{ .language = "rust", .command = "rust-analyzer" },
    .{ .language = "typescript", .command = "typescript-language-server", .args = &.{"--stdio"} },
    .{ .language = "go", .command = "gopls" },
    .{ .language = "python", .command = "pylsp" },
    .{ .language = "c", .command = "clangd" },
    .{ .language = "cpp", .command = "clangd" },
    .{ .language = "javascript", .command = "typescript-language-server", .args = &.{"--stdio"} },
};

const max_message_bytes = 4 * 1024 * 1024;

fn writeFrame(writer: *std.Io.Writer, payload: []const u8) !void {
    try writer.print("Content-Length: {d}\r\n\r\n", .{payload.len});
    try writer.writeAll(payload);
    try writer.flush();
}

fn readFrame(allocator: std.mem.Allocator, reader: *std.Io.Reader) ![]u8 {
    var content_length: ?usize = null;
    while (true) {
        const line = try reader.takeDelimiter('\n') orelse return error.LspClosed;
        const header = std.mem.trim(u8, line, " \r\n");
        if (header.len == 0) break;
        if (std.mem.startsWith(u8, header, "Content-Length:")) {
            const value = std.mem.trim(u8, header["Content-Length:".len..], " ");
            content_length = std.fmt.parseInt(usize, value, 10) catch return error.InvalidLspFrame;
        }
    }
    const len = content_length orelse return error.InvalidLspFrame;
    if (len > max_message_bytes) return error.LspFrameTooLarge;
    const payload = try allocator.alloc(u8, len);
    errdefer allocator.free(payload);
    try reader.readSliceAll(payload);
    return payload;
}

fn writeStringField(writer: *std.Io.Writer, name: []const u8, value: []const u8, leading_comma: bool) !void {
    if (leading_comma) try writer.writeAll(",");
    try std.json.Stringify.value(name, .{}, writer);
    try writer.writeAll(":");
    try std.json.Stringify.value(value, .{}, writer);
}

fn valueString(value: std.json.Value) ?[]const u8 {
    return switch (value) {
        .string => |s| s,
        else => null,
    };
}

fn valueInt(value: std.json.Value) ?u32 {
    return switch (value) {
        .integer => |n| if (n >= 0 and n <= std.math.maxInt(u32)) @intCast(n) else null,
        else => null,
    };
}

fn freeDiagnostics(allocator: std.mem.Allocator, diagnostics: []Diagnostic) void {
    for (diagnostics) |diagnostic| {
        allocator.free(diagnostic.uri);
        allocator.free(diagnostic.message);
        if (diagnostic.source) |source| allocator.free(source);
    }
    allocator.free(diagnostics);
}

pub const Client = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    language: []const u8,
    command: []const u8,
    args: []const []const u8,
    process: ?std.process.Child = null,
    stdout_reader: ?std.Io.File.Reader = null,
    stdout_buffer: [8192]u8 = undefined,
    initialized: bool = false,
    next_request_id: u64 = 1,
    diagnostics_by_uri: std.StringHashMap([]Diagnostic),

    pub fn init(allocator: std.mem.Allocator, io: std.Io, config: ServerConfig) !Client {
        const language = try allocator.dupe(u8, config.language);
        errdefer allocator.free(language);
        const command = try allocator.dupe(u8, config.command);
        return .{
            .allocator = allocator,
            .io = io,
            .language = language,
            .command = command,
            .args = config.args,
            .diagnostics_by_uri = std.StringHashMap([]Diagnostic).init(allocator),
        };
    }

    pub fn deinit(self: *Client) void {
        self.stop();
        var iter = self.diagnostics_by_uri.iterator();
        while (iter.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            freeDiagnostics(self.allocator, entry.value_ptr.*);
        }
        self.diagnostics_by_uri.deinit();
        self.allocator.free(self.language);
        self.allocator.free(self.command);
    }

    pub fn start(self: *Client) !void {
        var argv: std.ArrayList([]const u8) = .empty;
        defer argv.deinit(self.allocator);
        try argv.append(self.allocator, self.command);
        for (self.args) |arg| try argv.append(self.allocator, arg);
        self.process = try std.process.spawn(self.io, .{
            .argv = argv.items,
            .stdin = .pipe,
            .stdout = .pipe,
            .stderr = .inherit,
        });
        const stdout_file = self.process.?.stdout orelse return error.LspNotRunning;
        self.stdout_reader = .init(stdout_file, self.io, &self.stdout_buffer);
        log.info("started LSP for {s}: {s}", .{ self.language, self.command });
    }

    pub fn stop(self: *Client) void {
        self.stdout_reader = null;
        if (self.process) |*process| {
            process.kill(self.io);
            self.process = null;
        }
        self.initialized = false;
    }

    pub fn nextId(self: *Client) u64 {
        const id = self.next_request_id;
        self.next_request_id += 1;
        return id;
    }

    fn send(self: *Client, id: ?u64, method: []const u8, params_json: []const u8) !void {
        const process = self.process orelse return error.LspNotRunning;
        const stdin_file = process.stdin orelse return error.LspNotRunning;
        var output_buffer: [4096]u8 = undefined;
        var file_writer: std.Io.File.Writer = .init(stdin_file, self.io, &output_buffer);
        var payload: std.Io.Writer.Allocating = .init(self.allocator);
        defer payload.deinit();
        try payload.writer.writeAll("{\"jsonrpc\":\"2.0\"");
        if (id) |request_id| try payload.writer.print(",\"id\":{d}", .{request_id});
        try payload.writer.writeAll(",\"method\":");
        try std.json.Stringify.value(method, .{}, &payload.writer);
        try payload.writer.writeAll(",\"params\":");
        try payload.writer.writeAll(params_json);
        try payload.writer.writeAll("}");
        try writeFrame(&file_writer.interface, payload.written());
    }

    pub fn sendRequest(self: *Client, method: []const u8, params_json: []const u8) !u64 {
        const id = self.nextId();
        try self.send(id, method, params_json);
        return id;
    }

    pub fn sendNotification(self: *Client, method: []const u8, params_json: []const u8) !void {
        try self.send(null, method, params_json);
    }

    fn receive(self: *Client) ![]u8 {
        const reader = &(self.stdout_reader orelse return error.LspNotRunning).interface;
        return readFrame(self.allocator, reader);
    }

    fn replaceDiagnostics(self: *Client, uri: []const u8, entries: []Diagnostic) !void {
        if (self.diagnostics_by_uri.getPtr(uri)) |current| {
            freeDiagnostics(self.allocator, current.*);
            current.* = entries;
            return;
        }
        const owned_uri = try self.allocator.dupe(u8, uri);
        errdefer self.allocator.free(owned_uri);
        try self.diagnostics_by_uri.put(owned_uri, entries);
    }

    fn handleDiagnostics(self: *Client, params: std.json.Value) !void {
        const object = switch (params) {
            .object => |object| object,
            else => return error.InvalidLspNotification,
        };
        const uri = valueString(object.get("uri") orelse return error.InvalidLspNotification) orelse return error.InvalidLspNotification;
        const incoming = switch (object.get("diagnostics") orelse return error.InvalidLspNotification) {
            .array => |array| array.items,
            else => return error.InvalidLspNotification,
        };
        var entries: std.ArrayList(Diagnostic) = .empty;
        defer entries.deinit(self.allocator);
        errdefer {
            for (entries.items) |diagnostic| {
                self.allocator.free(diagnostic.uri);
                self.allocator.free(diagnostic.message);
                if (diagnostic.source) |source| self.allocator.free(source);
            }
        }
        for (incoming) |item| {
            const diagnostic_object = switch (item) {
                .object => |value| value,
                else => continue,
            };
            const range = switch (diagnostic_object.get("range") orelse continue) {
                .object => |value| value,
                else => continue,
            };
            const start_position = switch (range.get("start") orelse continue) {
                .object => |value| value,
                else => continue,
            };
            const line = valueInt(start_position.get("line") orelse continue) orelse continue;
            const character = valueInt(start_position.get("character") orelse continue) orelse continue;
            const message = valueString(diagnostic_object.get("message") orelse continue) orelse continue;
            const severity = if (diagnostic_object.get("severity")) |raw| switch (valueInt(raw) orelse 1) {
                1 => Diagnostic.Severity.err,
                2 => Diagnostic.Severity.warn,
                3 => Diagnostic.Severity.info,
                4 => Diagnostic.Severity.hint,
                else => Diagnostic.Severity.err,
            } else Diagnostic.Severity.err;
            const owned_uri = try self.allocator.dupe(u8, uri);
            errdefer self.allocator.free(owned_uri);
            const owned_message = try self.allocator.dupe(u8, message);
            errdefer self.allocator.free(owned_message);
            const source = if (diagnostic_object.get("source")) |raw| if (valueString(raw)) |value| try self.allocator.dupe(u8, value) else null else null;
            try entries.append(self.allocator, .{
                .uri = owned_uri,
                .line = line,
                .character = character,
                .severity = severity,
                .message = owned_message,
                .source = source,
            });
        }
        try self.replaceDiagnostics(uri, try entries.toOwnedSlice(self.allocator));
    }

    fn handleIncoming(self: *Client, payload: []const u8) !?u64 {
        const parsed = try std.json.parseFromSlice(std.json.Value, self.allocator, payload, .{});
        defer parsed.deinit();
        const object = switch (parsed.value) {
            .object => |object| object,
            else => return error.InvalidLspMessage,
        };
        if (object.get("method")) |method_value| {
            const method = valueString(method_value) orelse return error.InvalidLspMessage;
            if (std.mem.eql(u8, method, "textDocument/publishDiagnostics")) {
                try self.handleDiagnostics(object.get("params") orelse return error.InvalidLspNotification);
            }
            return null;
        }
        return if (object.get("id")) |id_value| switch (id_value) {
            .integer => |id| if (id >= 0) @intCast(id) else error.InvalidLspMessage,
            else => error.InvalidLspMessage,
        } else error.InvalidLspMessage;
    }

    fn request(self: *Client, method: []const u8, params_json: []const u8) ![]u8 {
        const id = try self.sendRequest(method, params_json);
        while (true) {
            const payload = try self.receive();
            errdefer self.allocator.free(payload);
            if (try self.handleIncoming(payload)) |response_id| {
                if (response_id == id) return payload;
            }
            self.allocator.free(payload);
        }
    }

    pub fn initialize(self: *Client, root_uri: []const u8) !void {
        var params: std.Io.Writer.Allocating = .init(self.allocator);
        defer params.deinit();
        try params.writer.writeAll("{\"processId\":null");
        try writeStringField(&params.writer, "rootUri", root_uri, true);
        try params.writer.writeAll(",\"capabilities\":{}}");
        const response = try self.request("initialize", params.written());
        defer self.allocator.free(response);
        try self.sendNotification("initialized", "{}");
        self.initialized = true;
        log.info("LSP {s} initialized", .{self.language});
    }

    pub fn didOpen(self: *Client, uri: []const u8, language_id: []const u8, text: []const u8, version: i64) !void {
        var params: std.Io.Writer.Allocating = .init(self.allocator);
        defer params.deinit();
        try params.writer.writeAll("{\"textDocument\":{");
        try writeStringField(&params.writer, "uri", uri, false);
        try writeStringField(&params.writer, "languageId", language_id, true);
        try params.writer.print(",\"version\":{d}", .{version});
        try writeStringField(&params.writer, "text", text, true);
        try params.writer.writeAll("}}");
        try self.sendNotification("textDocument/didOpen", params.written());
    }

    pub fn didChange(self: *Client, uri: []const u8, text: []const u8, version: i64) !void {
        var params: std.Io.Writer.Allocating = .init(self.allocator);
        defer params.deinit();
        try params.writer.writeAll("{\"textDocument\":{");
        try writeStringField(&params.writer, "uri", uri, false);
        try params.writer.print(",\"version\":{d}},\"contentChanges\":[{", .{version});
        try writeStringField(&params.writer, "text", text, false);
        try params.writer.writeAll("}]}");
        try self.sendNotification("textDocument/didChange", params.written());
    }

    pub fn didClose(self: *Client, uri: []const u8) !void {
        var params: std.Io.Writer.Allocating = .init(self.allocator);
        defer params.deinit();
        try params.writer.writeAll("{\"textDocument\":{");
        try writeStringField(&params.writer, "uri", uri, false);
        try params.writer.writeAll("}}");
        try self.sendNotification("textDocument/didClose", params.written());
    }

    pub fn diagnostics(self: *const Client, uri: []const u8) []const Diagnostic {
        return self.diagnostics_by_uri.get(uri) orelse &.{};
    }

    fn positionParams(self: *Client, uri: []const u8, line: u32, character: u32) ![]u8 {
        var params: std.Io.Writer.Allocating = .init(self.allocator);
        defer params.deinit();
        try params.writer.writeAll("{\"textDocument\":{");
        try writeStringField(&params.writer, "uri", uri, false);
        try params.writer.writeAll("},\"position\":{\"line\":");
        try params.writer.print("{d}", .{line});
        try params.writer.writeAll(",\"character\":");
        try params.writer.print("{d}", .{character});
        try params.writer.writeAll("}}");
        return try self.allocator.dupe(u8, params.written());
    }

    pub fn hover(self: *Client, uri: []const u8, line: u32, character: u32) !?Hover {
        const params = try self.positionParams(uri, line, character);
        defer self.allocator.free(params);
        const response = try self.request("textDocument/hover", params);
        defer self.allocator.free(response);
        const parsed = try std.json.parseFromSlice(std.json.Value, self.allocator, response, .{});
        defer parsed.deinit();
        const object = switch (parsed.value) {
            .object => |value| value,
            else => return error.InvalidLspMessage,
        };
        const result = object.get("result") orelse return null;
        const hover_object = switch (result) {
            .object => |value| value,
            .null => return null,
            else => return error.InvalidLspMessage,
        };
        const contents = hover_object.get("contents") orelse return null;
        const text = switch (contents) {
            .string => |value| value,
            .object => |value| valueString(value.get("value") orelse return null) orelse return null,
            else => return null,
        };
        return .{ .contents = try self.allocator.dupe(u8, text) };
    }

    fn appendLocation(self: *Client, locations: *std.ArrayList(Location), value: std.json.Value) !void {
        const object = switch (value) {
            .object => |item| item,
            else => return,
        };
        const uri = valueString(object.get("uri") orelse return) orelse return;
        const range = switch (object.get("range") orelse return) {
            .object => |item| item,
            else => return,
        };
        const start_position = switch (range.get("start") orelse return) {
            .object => |item| item,
            else => return,
        };
        const end = switch (range.get("end") orelse return) {
            .object => |item| item,
            else => return,
        };
        const start_line = valueInt(start_position.get("line") orelse return) orelse return;
        const start_character = valueInt(start_position.get("character") orelse return) orelse return;
        const end_line = valueInt(end.get("line") orelse return) orelse return;
        const end_character = valueInt(end.get("character") orelse return) orelse return;
        try locations.append(self.allocator, .{
            .uri = try self.allocator.dupe(u8, uri),
            .start_line = start_line,
            .start_character = start_character,
            .end_line = end_line,
            .end_character = end_character,
        });
    }

    pub fn definition(self: *Client, uri: []const u8, line: u32, character: u32) ![]Location {
        const params = try self.positionParams(uri, line, character);
        defer self.allocator.free(params);
        const response = try self.request("textDocument/definition", params);
        defer self.allocator.free(response);
        const parsed = try std.json.parseFromSlice(std.json.Value, self.allocator, response, .{});
        defer parsed.deinit();
        const object = switch (parsed.value) {
            .object => |value| value,
            else => return error.InvalidLspMessage,
        };
        const result = object.get("result") orelse return try self.allocator.alloc(Location, 0);
        var locations: std.ArrayList(Location) = .empty;
        errdefer {
            for (locations.items) |location| self.allocator.free(location.uri);
            locations.deinit(self.allocator);
        }
        switch (result) {
            .array => |items| for (items.items) |item| try self.appendLocation(&locations, item),
            .object => try self.appendLocation(&locations, result),
            .null => {},
            else => return error.InvalidLspMessage,
        }
        return locations.toOwnedSlice(self.allocator);
    }

    pub fn freeLocations(self: *Client, locations: []Location) void {
        for (locations) |location| self.allocator.free(location.uri);
        self.allocator.free(locations);
    }
};

pub const Manager = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    clients: std.StringHashMap(*Client),
    configs: std.StringHashMap(ServerConfig),

    pub fn init(allocator: std.mem.Allocator, io: std.Io) Manager {
        return .{
            .allocator = allocator,
            .io = io,
            .clients = std.StringHashMap(*Client).init(allocator),
            .configs = std.StringHashMap(ServerConfig).init(allocator),
        };
    }

    pub fn deinit(self: *Manager) void {
        var iter = self.clients.iterator();
        while (iter.next()) |entry| {
            entry.value_ptr.*.deinit();
            self.allocator.destroy(entry.value_ptr.*);
        }
        self.clients.deinit();
        self.configs.deinit();
    }

    pub fn registerServer(self: *Manager, config: ServerConfig) !void {
        try self.configs.put(config.language, config);
        log.info("registered LSP server for {s}: {s}", .{ config.language, config.command });
    }

    pub fn startForLanguage(self: *Manager, language: []const u8) !*Client {
        if (self.clients.get(language)) |client| return client;
        const config = self.configs.get(language) orelse blk: {
            for (default_servers) |default_config| {
                if (std.mem.eql(u8, default_config.language, language)) break :blk default_config;
            }
            return error.NoServerForLanguage;
        };
        const client = try self.allocator.create(Client);
        errdefer self.allocator.destroy(client);
        client.* = try Client.init(self.allocator, self.io, config);
        errdefer client.deinit();
        try client.start();
        try self.clients.put(language, client);
        return client;
    }

    pub fn get(self: *Manager, language: []const u8) ?*Client {
        return self.clients.get(language);
    }

    pub fn stopAll(self: *Manager) void {
        var iter = self.clients.iterator();
        while (iter.next()) |entry| entry.value_ptr.*.stop();
    }

    pub fn supportedLanguages(self: *const Manager, allocator: std.mem.Allocator) ![][]const u8 {
        var result: std.ArrayList([]const u8) = .empty;
        var iter = self.configs.iterator();
        while (iter.next()) |entry| try result.append(allocator, entry.key_ptr.*);
        for (default_servers) |default_config| {
            if (!self.configs.contains(default_config.language)) try result.append(allocator, default_config.language);
        }
        return result.toOwnedSlice(allocator);
    }
};

test "LSP frame round trip" {
    const gpa = std.testing.allocator;
    var output: std.Io.Writer.Allocating = .init(gpa);
    defer output.deinit();
    try writeFrame(&output.writer, "{\"id\":7}");
    var input: std.Io.Reader = .fixed(output.written());
    const frame = try readFrame(gpa, &input);
    defer gpa.free(frame);
    try std.testing.expectEqualStrings("{\"id\":7}", frame);
}

test "diagnostics notification replaces the URI cache" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();
    var client = try Client.init(gpa, io, .{ .language = "zig", .command = "zls" });
    defer client.deinit();
    const payload = "{\"jsonrpc\":\"2.0\",\"method\":\"textDocument/publishDiagnostics\",\"params\":{\"uri\":\"file:///tmp/a.zig\",\"diagnostics\":[{\"range\":{\"start\":{\"line\":2,\"character\":3}},\"severity\":2,\"message\":\"bad\",\"source\":\"zls\"}]}}";
    try std.testing.expect((try client.handleIncoming(payload)) == null);
    const diagnostics = client.diagnostics("file:///tmp/a.zig");
    try std.testing.expectEqual(@as(usize, 1), diagnostics.len);
    try std.testing.expectEqual(Diagnostic.Severity.warn, diagnostics[0].severity);
    try std.testing.expectEqualStrings("bad", diagnostics[0].message);
}

test "manager finds default server" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();
    var manager = Manager.init(gpa, io);
    defer manager.deinit();
    try manager.registerServer(.{ .language = "custom", .command = "custom-lsp" });
    const languages = try manager.supportedLanguages(gpa);
    defer gpa.free(languages);
    var found_zig = false;
    for (languages) |language| {
        if (std.mem.eql(u8, language, "zig")) found_zig = true;
    }
    try std.testing.expect(found_zig);
}
