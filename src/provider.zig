const std = @import("std");

const log = std.log.scoped(.provider);

pub const ProviderId = enum {
    openai,
    anthropic,
    google,
    local,

    pub fn defaultUrl(id: ProviderId) []const u8 {
        return switch (id) {
            .openai => "https://api.openai.com/v1",
            .anthropic => "https://api.anthropic.com/v1",
            .google => "https://generativelanguage.googleapis.com/v1beta",
            .local => "http://localhost:11434/v1",
        };
    }

    pub fn defaultModel(id: ProviderId) []const u8 {
        return switch (id) {
            .openai => "gpt-4o",
            .anthropic => "claude-sonnet-4-20250514",
            .google => "gemini-2.0-flash",
            .local => "llama3.1",
        };
    }
};

pub const Role = enum {
    user,
    assistant,
    system,
    tool,

    pub fn jsonStringify(self: Role, jw: *std.json.Stringify) !void {
        try jw.write(@tagName(self));
    }
};

pub const Message = struct {
    role: Role,
    content: []const u8,
};

pub const ToolDef = struct {
    type: []const u8 = "function",
    function: ToolFunction,
};

pub const ToolFunction = struct {
    name: []const u8,
    description: []const u8,
    parameters: []const u8,
};

pub const ChatRequest = struct {
    model: []const u8,
    messages: []const Message,
    stream: bool = false,
    temperature: ?f32 = null,
    max_tokens: ?u32 = null,
    tools: ?[]const ToolDef = null,
};

pub const ResponseToolCall = struct {
    id: []const u8 = "",
    type: []const u8 = "function",
    function: ResponseToolFunction = .{},
};

pub const ResponseToolFunction = struct {
    name: []const u8 = "",
    arguments: []const u8 = "",
};

pub const ChatChoice = struct {
    index: u32 = 0,
    message: ChatResponseMessage,
    finish_reason: ?[]const u8 = null,
};

pub const ChatResponseMessage = struct {
    role: Role,
    content: []const u8 = "",
    tool_calls: ?[]const ResponseToolCall = null,
};

pub const ChatResponse = struct {
    id: []const u8 = "",
    model: []const u8 = "",
    choices: []const ChatChoice = &.{},
};

pub const StreamDelta = struct {
    role: ?Role = null,
    content: ?[]const u8 = null,
};

pub const StreamChoice = struct {
    index: u32 = 0,
    delta: StreamDelta,
    finish_reason: ?[]const u8 = null,
};

pub const StreamChunk = struct {
    id: []const u8 = "",
    choices: []const StreamChoice = &.{},
};

pub const Provider = struct {
    id: ProviderId,
    base_url: []const u8,
    api_key: ?[]const u8,
    default_model: []const u8,
};

pub const Registry = struct {
    allocator: std.mem.Allocator,
    providers: std.ArrayList(Provider),

    pub fn init(allocator: std.mem.Allocator) Registry {
        return .{
            .allocator = allocator,
            .providers = std.ArrayList(Provider).empty,
        };
    }

    pub fn deinit(self: *Registry) void {
        for (self.providers.items) |provider| {
            if (provider.api_key) |api_key| self.allocator.free(api_key);
        }
        self.providers.deinit(self.allocator);
    }

    pub fn add(self: *Registry, id: ProviderId) !void {
        try self.providers.append(self.allocator, .{
            .id = id,
            .base_url = ProviderId.defaultUrl(id),
            .api_key = null,
            .default_model = ProviderId.defaultModel(id),
        });
        log.info("registered provider: {s}", .{@tagName(id)});
    }

    pub fn addWithKey(self: *Registry, id: ProviderId, api_key: []const u8) !void {
        try self.providers.append(self.allocator, .{
            .id = id,
            .base_url = ProviderId.defaultUrl(id),
            .api_key = try self.allocator.dupe(u8, api_key),
            .default_model = ProviderId.defaultModel(id),
        });
        log.info("registered provider: {s} (with key)", .{@tagName(id)});
    }

    pub fn count(self: *const Registry) usize {
        return self.providers.items.len;
    }

    pub fn get(self: *const Registry, id: ProviderId) ?Provider {
        for (self.providers.items) |provider| {
            if (provider.id == id) return provider;
        }
        return null;
    }
};

pub const HttpError = error{
    NetworkFailure,
    HttpStatusError,
    InvalidResponse,
    OutOfMemory,
    StreamParseError,
};

pub const Client = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    http_client: std.http.Client,
    last_response: ?std.json.Parsed(ChatResponse) = null,

    pub fn init(allocator: std.mem.Allocator, io: std.Io) Client {
        return .{
            .allocator = allocator,
            .io = io,
            .http_client = .{
                .allocator = allocator,
                .io = io,
            },
        };
    }

    pub fn deinit(self: *Client) void {
        if (self.last_response) |*response| response.deinit();
        self.http_client.deinit();
    }

    pub fn chatCompletion(
        self: *Client,
        provider: Provider,
        request: ChatRequest,
    ) !ChatResponse {
        var arena = std.heap.ArenaAllocator.init(self.allocator);
        defer arena.deinit();
        const arena_alloc = arena.allocator();

        var body_buf: std.Io.Writer.Allocating = .init(arena_alloc);
        const body_writer = &body_buf.writer;
        defer body_buf.deinit();

        try std.json.Stringify.value(request, .{}, body_writer);

        const url = try std.fmt.allocPrint(arena_alloc, "{s}/chat/completions", .{provider.base_url});

        var auth_header_buf: [256]u8 = undefined;
        const auth_header: ?std.http.Header = if (provider.api_key) |key| blk: {
            const value = try std.fmt.bufPrint(&auth_header_buf, "Bearer {s}", .{key});
            break :blk .{ .name = "Authorization", .value = value };
        } else null;

        var extra_headers: [2]std.http.Header = undefined;
        var header_count: usize = 0;
        extra_headers[header_count] = .{ .name = "Content-Type", .value = "application/json" };
        header_count += 1;
        if (auth_header) |ah| {
            extra_headers[header_count] = ah;
            header_count += 1;
        }

        var response_buf: std.Io.Writer.Allocating = .init(arena_alloc);
        const response_writer = &response_buf.writer;
        defer response_buf.deinit();

        const result = self.http_client.fetch(.{
            .location = .{ .url = url },
            .method = .POST,
            .payload = body_buf.written(),
            .extra_headers = extra_headers[0..header_count],
            .response_writer = response_writer,
        }) catch |err| {
            log.err("HTTP request failed: {}", .{err});
            return error.NetworkFailure;
        };

        if (result.status.class() != .success) {
            log.warn("HTTP {d}: {s}", .{ @intFromEnum(result.status), response_buf.written() });
            return error.HttpStatusError;
        }

        if (self.last_response) |*response| response.deinit();
        self.last_response = std.json.parseFromSlice(ChatResponse, self.allocator, response_buf.written(), .{
            .ignore_unknown_fields = true,
            .allocate = .alloc_always,
        }) catch |err| {
            log.err("JSON parse failed: {}", .{err});
            return error.InvalidResponse;
        };

        return self.last_response.?.value;
    }

    pub const StreamCallback = *const fn (ctx: ?*anyopaque, delta: []const u8) void;

    pub fn chatCompletionStream(
        self: *Client,
        provider: Provider,
        request: ChatRequest,
        ctx: ?*anyopaque,
        on_delta: StreamCallback,
    ) !void {
        var arena = std.heap.ArenaAllocator.init(self.allocator);
        defer arena.deinit();
        const arena_alloc = arena.allocator();

        var req = request;
        req.stream = true;

        var body_buf: std.Io.Writer.Allocating = .init(arena_alloc);
        const body_writer = &body_buf.writer;
        defer body_buf.deinit();

        try std.json.Stringify.value(req, .{}, body_writer);

        const url = try std.fmt.allocPrint(arena_alloc, "{s}/chat/completions", .{provider.base_url});

        var auth_header_buf: [256]u8 = undefined;
        const auth_header: ?std.http.Header = if (provider.api_key) |key| blk: {
            const value = try std.fmt.bufPrint(&auth_header_buf, "Bearer {s}", .{key});
            break :blk .{ .name = "Authorization", .value = value };
        } else null;

        var extra_headers: [2]std.http.Header = undefined;
        var header_count: usize = 0;
        extra_headers[header_count] = .{ .name = "Content-Type", .value = "application/json" };
        header_count += 1;
        if (auth_header) |ah| {
            extra_headers[header_count] = ah;
            header_count += 1;
        }

        var http_req = self.http_client.request(.POST, try std.Uri.parse(url), .{
            .extra_headers = extra_headers[0..header_count],
        }) catch |err| {
            log.err("HTTP request setup failed: {}", .{err});
            return error.NetworkFailure;
        };
        defer http_req.deinit();

        http_req.transfer_encoding = .{ .content_length = body_buf.written().len };
        var body_writer_req = http_req.sendBodyUnflushed(&.{}) catch |err| {
            log.err("HTTP send body failed: {}", .{err});
            return error.NetworkFailure;
        };
        try body_writer_req.writer.writeAll(body_buf.written());
        try body_writer_req.end();
        try http_req.connection.?.flush();

        var redirect_buf: [8192]u8 = undefined;
        var response = http_req.receiveHead(&redirect_buf) catch |err| {
            log.err("HTTP receive head failed: {}", .{err});
            return error.NetworkFailure;
        };

        if (response.head.status.class() != .success) {
            log.err("HTTP {d}", .{@intFromEnum(response.head.status)});
            return error.HttpStatusError;
        }

        var read_buf: [4096]u8 = undefined;
        const reader = response.reader(&read_buf);

        while (true) {
            const line = reader.takeDelimiter('\n') catch |err| {
                log.err("stream read failed: {}", .{err});
                return error.StreamParseError;
            };
            if (line == null) break;

            const trimmed = std.mem.trimEnd(u8, line.?, "\r");
            if (trimmed.len > 0) {
                try processSseLine(arena_alloc, trimmed, ctx, on_delta);
            }
        }
    }
};

fn processSseLine(
    arena: std.mem.Allocator,
    line: []const u8,
    ctx: ?*anyopaque,
    on_delta: Client.StreamCallback,
) !void {
    if (line.len == 0) return;
    if (std.mem.startsWith(u8, line, ":")) return;
    if (!std.mem.startsWith(u8, line, "data: ")) return;

    const data = line[6..];
    if (std.mem.eql(u8, data, "[DONE]")) return;

    const parsed = std.json.parseFromSlice(StreamChunk, arena, data, .{
        .ignore_unknown_fields = true,
    }) catch return;

    for (parsed.value.choices) |choice| {
        if (choice.delta.content) |content| {
            on_delta(ctx, content);
        }
    }
}

pub fn serializeChatRequest(allocator: std.mem.Allocator, request: ChatRequest) ![]u8 {
    var buf: std.Io.Writer.Allocating = .init(allocator);
    const writer = &buf.writer;
    defer buf.deinit();
    try std.json.Stringify.value(request, .{}, writer);
    return try allocator.dupe(u8, buf.written());
}

test "registry adds providers" {
    const gpa = std.testing.allocator;
    var registry = Registry.init(gpa);
    defer registry.deinit();

    try registry.add(.openai);
    try registry.add(.anthropic);

    try std.testing.expectEqual(@as(usize, 2), registry.count());
    try std.testing.expect(registry.get(.openai) != null);
}

test "registry frees owned keys" {
    const gpa = std.testing.allocator;
    var registry = Registry.init(gpa);
    defer registry.deinit();

    try registry.addWithKey(.openai, "test-key");
    try std.testing.expectEqualStrings("test-key", registry.get(.openai).?.api_key.?);
}

test "serialize chat request" {
    const gpa = std.testing.allocator;
    const messages = [_]Message{
        .{ .role = .system, .content = "You are helpful." },
        .{ .role = .user, .content = "Hello" },
    };
    const req = ChatRequest{
        .model = "gpt-4o",
        .messages = &messages,
        .stream = false,
    };
    const json = try serializeChatRequest(gpa, req);
    defer gpa.free(json);

    try std.testing.expect(std.mem.indexOf(u8, json, "\"model\":\"gpt-4o\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"role\":\"user\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"content\":\"Hello\"") != null);
}

test "parse stream chunk" {
    const gpa = std.testing.allocator;
    const data =
        \\{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":"Hi"},"finish_reason":null}]}
    ;
    const parsed = try std.json.parseFromSlice(StreamChunk, gpa, data, .{
        .ignore_unknown_fields = true,
    });
    defer parsed.deinit();

    try std.testing.expectEqual(@as(usize, 1), parsed.value.choices.len);
    try std.testing.expectEqualStrings("Hi", parsed.value.choices[0].delta.content.?);
}

test "process sse line extracts content" {
    const gpa = std.testing.allocator;
    var arena = std.heap.ArenaAllocator.init(gpa);
    defer arena.deinit();

    var captured: []const u8 = "";
    const ctx: ?*anyopaque = @ptrCast(&captured);
    const cb = struct {
        fn fn_(c: ?*anyopaque, delta: []const u8) void {
            const ptr: *[]const u8 = @ptrCast(@alignCast(c.?));
            ptr.* = delta;
        }
    }.fn_;

    try processSseLine(arena.allocator(),
        \\data: {"choices":[{"delta":{"content":"world"}}]}
    , ctx, cb);
    try std.testing.expectEqualStrings("world", captured);
}

test "chat completion end-to-end with mock server" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();

    // Start mock HTTP server on localhost:0 (ephemeral port)
    var server_addr = try std.Io.net.IpAddress.parseIp4("127.0.0.1", 0);
    var server = try server_addr.listen(io, .{ .reuse_address = true });
    defer server.deinit(io);

    const port = server.socket.address.getPort();
    try std.testing.expect(port > 0);

    // Spawn server thread: accept one connection, respond with mock JSON
    const server_handle = try std.Thread.spawn(.{}, struct {
        fn run(s: *std.Io.net.Server, server_io: std.Io) void {
            const stream = s.accept(server_io) catch return;
            defer stream.close(server_io);

            var read_buf: [4096]u8 = undefined;
            var file_reader = std.Io.net.Stream.Reader.init(stream, server_io, &read_buf);
            const reader = &file_reader.interface;

            // Skip request line and headers (just drain newlines)
            while (true) {
                const line = reader.takeDelimiter('\n') catch break;
                if (line == null) break;
                const trimmed = std.mem.trim(u8, line.?, " \r\n");
                if (trimmed.len == 0) break;
            }

            // Mock response: OpenAI-compatible
            const response_body =
                \\{"id":"chatcmpl-mock123","object":"chat.completion","created":1234567890,"model":"gpt-4o","choices":[{"index":0,"message":{"role":"assistant","content":"Mock response from test server"},"finish_reason":"stop"}]}
            ;

            const status_line = "HTTP/1.1 200 OK\r\n";
            var cl_buf: [64]u8 = undefined;
            const content_len_str = std.fmt.bufPrint(&cl_buf, "Content-Length: {d}\r\n", .{response_body.len}) catch return;

            var write_buf: [4096]u8 = undefined;
            var file_writer = std.Io.net.Stream.Writer.init(stream, server_io, &write_buf);
            const writer = &file_writer.interface;

            writer.writeAll(status_line) catch return;
            writer.writeAll("Content-Type: application/json\r\n") catch return;
            writer.writeAll(content_len_str) catch return;
            writer.writeAll("Connection: close\r\n") catch return;
            writer.writeAll("\r\n") catch return;
            writer.writeAll(response_body) catch return;
            writer.flush() catch return;
        }
    }.run, .{ &server, io });
    server_handle.detach();

    // Give server thread time to start listening
    std.Io.sleep(io, .fromMilliseconds(10), .awake) catch {};

    // Create client and provider pointing to our mock
    var client = Client.init(gpa, io);
    defer client.deinit();

    const base_url = try std.fmt.allocPrint(gpa, "http://127.0.0.1:{d}", .{port});
    defer gpa.free(base_url);

    const provider = Provider{
        .id = .local,
        .base_url = base_url,
        .api_key = "sk-test-key",
        .default_model = "gpt-4o",
    };

    const messages = [_]Message{
        .{ .role = .user, .content = "Hello" },
    };
    const request = ChatRequest{
        .model = "gpt-4o",
        .messages = &messages,
        .stream = false,
    };

    const response = try client.chatCompletion(provider, request);

    try std.testing.expectEqualStrings("chatcmpl-mock123", response.id);
    try std.testing.expect(response.choices.len > 0);
    try std.testing.expectEqualStrings("Mock response from test server", response.choices[0].message.content);
    try std.testing.expectEqualStrings("assistant", @tagName(response.choices[0].message.role));
}

test "chat completion returns error status codes" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();

    var server_addr = try std.Io.net.IpAddress.parseIp4("127.0.0.1", 0);
    var server = try server_addr.listen(io, .{ .reuse_address = true });
    defer server.deinit(io);

    const port = server.socket.address.getPort();

    const server_handle = try std.Thread.spawn(.{}, struct {
        fn run(s: *std.Io.net.Server, server_io: std.Io) void {
            const stream = s.accept(server_io) catch return;
            defer stream.close(server_io);

            // Skip request
            var read_buf: [4096]u8 = undefined;
            var file_reader = std.Io.net.Stream.Reader.init(stream, server_io, &read_buf);
            const reader = &file_reader.interface;
            while (true) {
                const line = reader.takeDelimiter('\n') catch break;
                if (line == null) break;
                const trimmed = std.mem.trim(u8, line.?, " \r\n");
                if (trimmed.len == 0) break;
            }

            const status_line = "HTTP/1.1 400 Bad Request\r\n";
            const body = "{\"error\":{\"message\":\"Bad request\",\"type\":\"invalid_request_error\"}}";
            var cl_buf: [64]u8 = undefined;
            const content_len_str = std.fmt.bufPrint(&cl_buf, "Content-Length: {d}\r\n", .{body.len}) catch return;

            var write_buf: [4096]u8 = undefined;
            var file_writer = std.Io.net.Stream.Writer.init(stream, server_io, &write_buf);
            const writer = &file_writer.interface;

            writer.writeAll(status_line) catch return;
            writer.writeAll(content_len_str) catch return;
            writer.writeAll("Content-Type: application/json\r\n") catch return;
            writer.writeAll("Connection: close\r\n") catch return;
            writer.writeAll("\r\n") catch return;
            writer.writeAll(body) catch return;
            writer.flush() catch return;
        }
    }.run, .{ &server, io });
    server_handle.detach();

    var client = Client.init(gpa, io);
    defer client.deinit();

    const base_url = try std.fmt.allocPrint(gpa, "http://127.0.0.1:{d}", .{port});
    defer gpa.free(base_url);

    const provider = Provider{
        .id = .local,
        .base_url = base_url,
        .api_key = "sk-test-key",
        .default_model = "gpt-4o",
    };

    const request = ChatRequest{
        .model = "gpt-4o",
        .messages = &.{.{ .role = .user, .content = "Hello" }},
        .stream = false,
    };

    const result = client.chatCompletion(provider, request);
    try std.testing.expectError(error.HttpStatusError, result);
}
