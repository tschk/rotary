const std = @import("std");
const provider = @import("provider.zig");
const permissions = @import("permissions.zig");
const hooks = @import("hooks.zig");

const log = std.log.scoped(.agent);

pub const Role = provider.Role;
pub const Message = provider.Message;

pub const ToolCall = struct {
    id: []const u8,
    name: []const u8,
    arguments: []const u8,
};

pub const ToolResult = struct {
    id: []const u8,
    content: []const u8,
    is_error: bool,
};

pub const ToolDefinition = struct {
    name: []const u8,
    description: []const u8,
    parameters_json: []const u8,
    execute: *const fn (ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) anyerror!ToolResult,
    ctx: ?*anyopaque = null,
};

pub const Event = union(enum) {
    before_agent_start: BeforeAgentStart,
    agent_start,
    turn_start,
    message_start: Message,
    message_update: []const u8,
    message_end: Message,
    tool_call: ToolCall,
    tool_execution_start: ToolCall,
    tool_execution_update: []const u8,
    tool_execution_end: ToolResult,
    tool_result: ToolResult,
    turn_end,
    agent_end,

    pub const BeforeAgentStart = struct {
        messages: []const Message,
        system_prompt: ?[]const u8 = null,
    };
};

pub const Subscriber = *const fn (ctx: ?*anyopaque, event: Event) void;

pub const ToolRegistry = struct {
    allocator: std.mem.Allocator,
    tools: std.StringHashMap(ToolDefinition),

    pub fn init(allocator: std.mem.Allocator) ToolRegistry {
        return .{
            .allocator = allocator,
            .tools = std.StringHashMap(ToolDefinition).init(allocator),
        };
    }

    pub fn deinit(self: *ToolRegistry) void {
        self.tools.deinit();
    }

    pub fn register(self: *ToolRegistry, tool: ToolDefinition) !void {
        try self.tools.put(tool.name, tool);
        log.info("registered tool: {s}", .{tool.name});
    }

    pub fn get(self: *const ToolRegistry, name: []const u8) ?ToolDefinition {
        return self.tools.get(name);
    }

    pub fn count(self: *const ToolRegistry) usize {
        return self.tools.count();
    }

    pub fn iterator(self: *const ToolRegistry) std.StringHashMap(ToolDefinition).Iterator {
        return self.tools.iterator();
    }
};

pub const Agent = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    provider_client: ?*provider.Client = null,
    provider_config: ?provider.Provider = null,
    model: []const u8 = "gpt-4o",
    system_prompt: ?[]const u8 = null,
    tools: ?*ToolRegistry = null,
    policy: permissions.Policy = .{ .mode = .full_access },
    approver_ctx: ?*anyopaque = null,
    approver: ?permissions.Approver = null,
    hooks_registry: ?*hooks.Registry = null,
    max_tool_iterations: usize = 20,
    auto_compact_after: usize = 80,
    subscribers: std.ArrayList(SubscriberEntry),
    messages: std.ArrayList(Message),

    pub const SubscriberEntry = struct {
        ctx: ?*anyopaque,
        callback: Subscriber,
    };

    pub fn init(allocator: std.mem.Allocator, io: std.Io) Agent {
        return .{
            .allocator = allocator,
            .io = io,
            .subscribers = std.ArrayList(SubscriberEntry).empty,
            .messages = std.ArrayList(Message).empty,
        };
    }

    pub fn deinit(self: *Agent) void {
        for (self.messages.items) |message| {
            self.allocator.free(message.content);
        }
        self.messages.deinit(self.allocator);
        self.subscribers.deinit(self.allocator);
    }

    pub fn clearMessages(self: *Agent) void {
        for (self.messages.items) |message| {
            self.allocator.free(message.content);
        }
        self.messages.clearRetainingCapacity();
    }

    pub fn setProvider(self: *Agent, client: *provider.Client, config: provider.Provider) void {
        self.provider_client = client;
        self.provider_config = config;
        self.model = config.default_model;
    }

    pub fn setModel(self: *Agent, model: []const u8) void {
        self.model = model;
    }

    pub fn setSystemPrompt(self: *Agent, sp: []const u8) void {
        self.system_prompt = sp;
    }

    pub fn setTools(self: *Agent, registry: *ToolRegistry) void {
        self.tools = registry;
    }

    pub fn setPolicy(self: *Agent, policy: permissions.Policy) void {
        self.policy = policy;
    }

    pub fn setApprover(self: *Agent, ctx: ?*anyopaque, approver: permissions.Approver) void {
        self.approver_ctx = ctx;
        self.approver = approver;
    }

    pub fn setHooks(self: *Agent, registry: *hooks.Registry) void {
        self.hooks_registry = registry;
    }

    pub fn subscribe(self: *Agent, ctx: ?*anyopaque, callback: Subscriber) !void {
        try self.subscribers.append(self.allocator, .{ .ctx = ctx, .callback = callback });
    }

    pub fn prompt(self: *Agent, text: []const u8) !void {
        if (self.hooks_registry) |hr| {
            var hev: hooks.HookEvent = .{ .before_prompt = text };
            hr.run(.before_prompt, &hev);
        }

        if (self.messages.items.len >= self.auto_compact_after) {
            try self.compact("auto-compact before prompt");
        }

        const owned = try self.allocator.dupe(u8, text);
        errdefer self.allocator.free(owned);
        try self.messages.append(self.allocator, .{ .role = .user, .content = owned });

        self.emit(.{ .before_agent_start = .{
            .messages = self.messages.items,
            .system_prompt = self.system_prompt,
        } });
        self.emit(.agent_start);
        try self.runTurn();
        if (self.hooks_registry) |hr| {
            var hev: hooks.HookEvent = .after_turn;
            hr.run(.after_turn, &hev);
        }
        self.emit(.agent_end);
    }

    /// Compact conversation history in place. Keeps first 2 + last 12 messages.
    pub fn compact(self: *Agent, summary: []const u8) !void {
        if (self.hooks_registry) |hr| {
            var hev: hooks.HookEvent = .{ .before_compact = self.messages.items.len };
            hr.run(.before_compact, &hev);
        }
        const context = @import("context.zig");
        const compacted = try context.compactMessages(self.allocator, self.messages.items, 2, 12, summary);
        defer {
            for (compacted) |m| self.allocator.free(m.content);
            self.allocator.free(compacted);
        }
        for (self.messages.items) |message| {
            self.allocator.free(message.content);
        }
        self.messages.clearRetainingCapacity();
        for (compacted) |message| {
            const owned = try self.allocator.dupe(u8, message.content);
            try self.messages.append(self.allocator, .{ .role = message.role, .content = owned });
        }
        log.info("compacted conversation to {d} messages", .{self.messages.items.len});
    }

    fn runTurn(self: *Agent) !void {
        self.emit(.turn_start);

        if (self.provider_client == null or self.provider_config == null) {
            const fallback = try self.allocator.dupe(u8, "[no provider configured]");
            self.emit(.{ .message_start = .{ .role = .assistant, .content = fallback } });
            self.emit(.{ .message_end = .{ .role = .assistant, .content = fallback } });
            try self.messages.append(self.allocator, .{ .role = .assistant, .content = fallback });
            self.emit(.turn_end);
            return;
        }

        const client = self.provider_client.?;
        const config = self.provider_config.?;

        // Build tool definitions if a tool registry is set
        var tool_defs: std.ArrayList(provider.ToolDef) = .empty;
        defer tool_defs.deinit(self.allocator);

        if (self.tools) |registry| {
            var it = registry.iterator();
            while (it.next()) |entry| {
                const tool = entry.value_ptr.*;
                try tool_defs.append(self.allocator, .{
                    .function = .{
                        .name = tool.name,
                        .description = tool.description,
                        .parameters = tool.parameters_json,
                    },
                });
            }
        }

        const has_tools = tool_defs.items.len > 0;
        const max_iterations = self.max_tool_iterations;

        var iteration: usize = 0;
        while (iteration < max_iterations) : (iteration += 1) {
            var all_messages: std.ArrayList(Message) = .empty;
            defer all_messages.deinit(self.allocator);

            if (self.system_prompt) |sp| {
                try all_messages.append(self.allocator, .{ .role = .system, .content = sp });
            }
            for (self.messages.items) |msg| {
                try all_messages.append(self.allocator, msg);
            }

            const request = ChatRequest{
                .model = self.model,
                .messages = all_messages.items,
                .stream = !has_tools,
                .tools = if (has_tools) tool_defs.items else null,
            };

            if (has_tools) {
                // Non-streaming path for tool calls
                const response = client.chatCompletion(config, request) catch |err| {
                    log.err("provider request failed: {}", .{err});
                    const err_msg = try std.fmt.allocPrint(self.allocator, "[provider error: {}]", .{err});
                    self.emit(.{ .message_start = .{ .role = .assistant, .content = err_msg } });
                    self.emit(.{ .message_end = .{ .role = .assistant, .content = err_msg } });
                    try self.messages.append(self.allocator, .{ .role = .assistant, .content = err_msg });
                    self.emit(.turn_end);
                    return;
                };

                const choice = if (response.choices.len > 0) response.choices[0] else {
                    self.emit(.turn_end);
                    return;
                };

                // If there are tool calls, execute them and continue
                if (choice.message.tool_calls) |tool_calls| {
                    if (tool_calls.len > 0) {
                        // Add assistant message with tool call info
                        const assistant_content = try self.allocator.dupe(u8, choice.message.content);
                        try self.messages.append(self.allocator, .{ .role = .assistant, .content = assistant_content });
                        self.emit(.{ .message_start = .{ .role = .assistant, .content = assistant_content } });
                        self.emit(.{ .message_end = .{ .role = .assistant, .content = assistant_content } });

                        // Execute each tool call
                        for (tool_calls) |tc| {
                            const call = ToolCall{
                                .id = try self.allocator.dupe(u8, tc.id),
                                .name = try self.allocator.dupe(u8, tc.function.name),
                                .arguments = try self.allocator.dupe(u8, tc.function.arguments),
                            };
                            try self.executeTool(call);

                            // Add tool result to messages
                            if (self.tools) |registry| {
                                if (registry.get(call.name)) |tool| {
                                    const result = tool.execute(tool.ctx, self.allocator, call.arguments) catch |err| {
                                        const err_content = try std.fmt.allocPrint(self.allocator, "tool error: {}", .{err});
                                        try self.messages.append(self.allocator, .{ .role = .tool, .content = err_content });
                                        continue;
                                    };
                                    try self.messages.append(self.allocator, .{ .role = .tool, .content = result.content });
                                }
                            }
                        }
                        continue; // Continue the loop for the next LLM response
                    }
                }

                // No tool calls — final response
                const content = try self.allocator.dupe(u8, choice.message.content);
                self.emit(.{ .message_start = .{ .role = .assistant, .content = content } });
                self.emit(.{ .message_end = .{ .role = .assistant, .content = content } });
                try self.messages.append(self.allocator, .{ .role = .assistant, .content = content });
                break;
            } else {
                // Streaming path (no tools)
                var stream_ctx = StreamCtx{
                    .agent = self,
                    .accumulated = .empty,
                };
                defer stream_ctx.accumulated.deinit(self.allocator);

                client.chatCompletionStream(config, request, &stream_ctx, onStreamDelta) catch |err| {
                    log.err("provider stream failed: {}", .{err});
                    const err_msg = try std.fmt.allocPrint(self.allocator, "[provider error: {}]", .{err});
                    self.emit(.{ .message_start = .{ .role = .assistant, .content = err_msg } });
                    self.emit(.{ .message_end = .{ .role = .assistant, .content = err_msg } });
                    try self.messages.append(self.allocator, .{ .role = .assistant, .content = err_msg });
                    self.emit(.turn_end);
                    return;
                };

                if (stream_ctx.accumulated.items.len > 0) {
                    const full = try self.allocator.dupe(u8, stream_ctx.accumulated.items);
                    self.emit(.{ .message_end = .{ .role = .assistant, .content = full } });
                    try self.messages.append(self.allocator, .{ .role = .assistant, .content = full });
                }
                break;
            }
        }

        self.emit(.turn_end);
    }

    const StreamCtx = struct {
        agent: *Agent,
        accumulated: std.ArrayList(u8),
    };

    fn onStreamDelta(ctx: ?*anyopaque, delta: []const u8) void {
        const sc: *StreamCtx = @ptrCast(@alignCast(ctx.?));
        sc.agent.emit(.{ .message_update = delta });
        sc.accumulated.appendSlice(sc.agent.allocator, delta) catch return;
    }

    pub fn executeTool(self: *Agent, call: ToolCall) !void {
        self.emit(.{ .tool_call = call });

        const req = permissions.Request{
            .tool_name = call.name,
            .arguments = call.arguments,
        };
        var before: hooks.HookEvent = .{ .before_tool = .{
            .request = req,
            .decision = permissions.authorize(self.policy, req, self.approver_ctx, self.approver),
        } };
        if (self.hooks_registry) |hr| {
            hr.run(.before_tool, &before);
        }
        const decision = before.before_tool.decision;
        if (decision == .deny or decision == .ask) {
            const err_content = try std.fmt.allocPrint(self.allocator, "tool denied by policy: {s}", .{call.name});
            defer self.allocator.free(err_content);
            const err_result = ToolResult{
                .id = call.id,
                .content = err_content,
                .is_error = true,
            };
            self.emit(.{ .tool_execution_end = err_result });
            self.emit(.{ .tool_result = err_result });
            return;
        }

        if (self.tools) |registry| {
            if (registry.get(call.name)) |tool| {
                self.emit(.{ .tool_execution_start = call });
                const result = tool.execute(tool.ctx, self.allocator, call.arguments) catch |err| {
                    const err_content = try std.fmt.allocPrint(self.allocator, "tool execution failed: {}", .{err});
                    defer self.allocator.free(err_content);
                    const err_result = ToolResult{
                        .id = call.id,
                        .content = err_content,
                        .is_error = true,
                    };
                    if (self.hooks_registry) |hr| {
                        var hev: hooks.HookEvent = .{ .on_error = err_content };
                        hr.run(.on_error, &hev);
                    }
                    self.emit(.{ .tool_execution_end = err_result });
                    self.emit(.{ .tool_result = err_result });
                    return;
                };
                defer self.allocator.free(result.content);
                if (self.hooks_registry) |hr| {
                    var hev: hooks.HookEvent = .{ .after_tool = .{
                        .tool_name = call.name,
                        .result = result,
                    } };
                    hr.run(.after_tool, &hev);
                }
                self.emit(.{ .tool_execution_end = result });
                self.emit(.{ .tool_result = result });
            } else {
                const err_content = try std.fmt.allocPrint(self.allocator, "unknown tool: {s}", .{call.name});
                defer self.allocator.free(err_content);
                const err_result = ToolResult{
                    .id = call.id,
                    .content = err_content,
                    .is_error = true,
                };
                self.emit(.{ .tool_execution_end = err_result });
                self.emit(.{ .tool_result = err_result });
            }
        } else {
            const err_result = ToolResult{
                .id = call.id,
                .content = "no tool registry configured",
                .is_error = true,
            };
            self.emit(.{ .tool_execution_end = err_result });
            self.emit(.{ .tool_result = err_result });
        }
    }

    fn emit(self: *Agent, event: Event) void {
        for (self.subscribers.items) |entry| {
            entry.callback(entry.ctx, event);
        }
        if (self.subscribers.items.len == 0) {
            log.info("{}", .{event});
        }
    }
};

const ChatRequest = provider.ChatRequest;

test "agent prompt emits full lifecycle" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();
    var agent = Agent.init(gpa, io);
    defer agent.deinit();

    var seen = std.ArrayList(EventTag).empty;
    defer seen.deinit(gpa);

    var ctx = TestCtx{ .seen = &seen, .gpa = gpa };
    try agent.subscribe(&ctx, testCallback);

    try agent.prompt("hi");

    try std.testing.expect(seen.items.len >= 5);
    try std.testing.expectEqual(EventTag.before_agent_start, seen.items[0]);
    try std.testing.expectEqual(EventTag.agent_start, seen.items[1]);
    try std.testing.expectEqual(EventTag.turn_start, seen.items[2]);
    try std.testing.expectEqual(EventTag.agent_end, seen.items[seen.items.len - 1]);
}

const EventTag = std.meta.Tag(Event);

const TestCtx = struct {
    seen: *std.ArrayList(EventTag),
    gpa: std.mem.Allocator,
};

fn testCallback(ctx: ?*anyopaque, event: Event) void {
    const tc: *TestCtx = @ptrCast(@alignCast(ctx.?));
    tc.seen.append(tc.gpa, std.meta.activeTag(event)) catch return;
}

test "tool registry register and get" {
    const gpa = std.testing.allocator;
    var registry = ToolRegistry.init(gpa);
    defer registry.deinit();

    const tool = ToolDefinition{
        .name = "read_file",
        .description = "Read a file",
        .parameters_json = "{}",
        .execute = dummyExecute,
    };
    try registry.register(tool);
    try std.testing.expect(registry.get("read_file") != null);
    try std.testing.expect(registry.get("nonexistent") == null);
}

fn dummyExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !ToolResult {
    _ = ctx;
    _ = arguments;
    return ToolResult{
        .id = "test",
        .content = try allocator.dupe(u8, "ok"),
        .is_error = false,
    };
}

test "agent executes tool via registry" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.io();
    var agent = Agent.init(gpa, io);
    defer agent.deinit();

    var registry = ToolRegistry.init(gpa);
    defer registry.deinit();
    try registry.register(.{
        .name = "echo",
        .description = "echo back",
        .parameters_json = "{}",
        .execute = echoExecute,
    });
    agent.setTools(&registry);

    var results: std.ArrayList(ToolResult) = .empty;
    defer {
        for (results.items) |r| gpa.free(r.content);
        results.deinit(gpa);
    }
    var ctx = ToolTestCtx{ .results = &results, .gpa = gpa };
    try agent.subscribe(&ctx, toolTestCallback);

    try agent.executeTool(.{ .id = "call-1", .name = "echo", .arguments = "{}" });

    try std.testing.expectEqual(@as(usize, 1), results.items.len);
    try std.testing.expectEqualStrings("echo:{}", results.items[0].content);
    try std.testing.expect(!results.items[0].is_error);
}

const ToolTestCtx = struct {
    results: *std.ArrayList(ToolResult),
    gpa: std.mem.Allocator,
};

fn toolTestCallback(ctx: ?*anyopaque, event: Event) void {
    const tc: *ToolTestCtx = @ptrCast(@alignCast(ctx.?));
    switch (event) {
        .tool_result => |r| {
            const content = tc.gpa.dupe(u8, r.content) catch return;
            tc.results.append(tc.gpa, .{
                .id = r.id,
                .content = content,
                .is_error = r.is_error,
            }) catch {
                tc.gpa.free(content);
            };
        },
        else => {},
    }
}

fn echoExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !ToolResult {
    _ = ctx;
    return ToolResult{
        .id = "call-1",
        .content = try std.fmt.allocPrint(allocator, "echo:{s}", .{arguments}),
        .is_error = false,
    };
}
