const std = @import("std");
const agent = @import("agent.zig");
const permissions = @import("permissions.zig");

const log = std.log.scoped(.hooks);

/// Lifecycle hooks inspired by dot (22 events) and zero plugins/hooks —
/// subset focused on harness control points.
pub const Phase = enum {
    before_prompt,
    before_tool,
    after_tool,
    before_compact,
    after_turn,
    on_error,
};

pub const BeforeTool = struct {
    request: permissions.Request,
    decision: permissions.Decision = .allow,
};

pub const AfterTool = struct {
    tool_name: []const u8,
    result: agent.ToolResult,
};

pub const HookEvent = union(enum) {
    before_prompt: []const u8,
    before_tool: BeforeTool,
    after_tool: AfterTool,
    before_compact: usize,
    after_turn,
    on_error: []const u8,
};

pub const HookFn = *const fn (ctx: ?*anyopaque, event: *HookEvent) void;

pub const Registry = struct {
    allocator: std.mem.Allocator,
    hooks: std.ArrayList(Entry),

    pub const Entry = struct {
        phase: Phase,
        ctx: ?*anyopaque,
        callback: HookFn,
    };

    pub fn init(allocator: std.mem.Allocator) Registry {
        return .{
            .allocator = allocator,
            .hooks = std.ArrayList(Entry).empty,
        };
    }

    pub fn deinit(self: *Registry) void {
        self.hooks.deinit(self.allocator);
    }

    pub fn add(self: *Registry, phase: Phase, ctx: ?*anyopaque, callback: HookFn) !void {
        try self.hooks.append(self.allocator, .{
            .phase = phase,
            .ctx = ctx,
            .callback = callback,
        });
        log.info("registered hook for {}", .{phase});
    }

    pub fn run(self: *const Registry, phase: Phase, event: *HookEvent) void {
        for (self.hooks.items) |entry| {
            if (entry.phase == phase) {
                entry.callback(entry.ctx, event);
            }
        }
    }
};

test "hooks fire for phase" {
    const gpa = std.testing.allocator;
    var reg = Registry.init(gpa);
    defer reg.deinit();

    var count: usize = 0;
    const cb = struct {
        fn f(ctx: ?*anyopaque, event: *HookEvent) void {
            _ = event;
            const c: *usize = @ptrCast(@alignCast(ctx.?));
            c.* += 1;
        }
    }.f;
    try reg.add(.after_turn, &count, cb);
    var ev: HookEvent = .after_turn;
    reg.run(.after_turn, &ev);
    try std.testing.expectEqual(@as(usize, 1), count);
}
