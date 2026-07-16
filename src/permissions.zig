const std = @import("std");

const log = std.log.scoped(.permissions);

/// Tool permission modes inspired by Zero/Codex/openclaude sandbox policies.
pub const Mode = enum {
    /// Allow every tool call without prompting.
    full_access,
    /// Allow filesystem reads; require approval for writes, shell, and network.
    read_only,
    /// Allow workspace-scoped filesystem ops; require approval for shell and network.
    workspace_write,
    /// Deny all tools (observer mode).
    deny_all,
};

pub const Decision = enum {
    allow,
    deny,
    ask,
};

pub const Request = struct {
    tool_name: []const u8,
    arguments: []const u8,
    workspace_root: ?[]const u8 = null,
};

pub const Policy = struct {
    mode: Mode = .workspace_write,
    /// Tools always allowed regardless of mode.
    allowlist: []const []const u8 = &.{},
    /// Tools always denied.
    denylist: []const []const u8 = &.{},

    pub fn decide(self: Policy, req: Request) Decision {
        for (self.denylist) |name| {
            if (std.mem.eql(u8, name, req.tool_name)) return .deny;
        }
        for (self.allowlist) |name| {
            if (std.mem.eql(u8, name, req.tool_name)) return .allow;
        }

        return switch (self.mode) {
            .full_access => .allow,
            .deny_all => .deny,
            .read_only => if (isReadTool(req.tool_name)) .allow else .ask,
            .workspace_write => if (isDangerousTool(req.tool_name)) .ask else .allow,
        };
    }
};

pub fn isReadTool(name: []const u8) bool {
    return std.mem.eql(u8, name, "read_file") or
        std.mem.eql(u8, name, "list_dir") or
        std.mem.eql(u8, name, "find_files") or
        std.mem.eql(u8, name, "code_intel");
}

pub fn isWriteTool(name: []const u8) bool {
    return std.mem.eql(u8, name, "write_file");
}

pub fn isShellTool(name: []const u8) bool {
    return std.mem.eql(u8, name, "run_command") or
        std.mem.eql(u8, name, "spawn_agent");
}

pub fn isDangerousTool(name: []const u8) bool {
    return isShellTool(name) or isWriteTool(name);
}

pub const Approver = *const fn (ctx: ?*anyopaque, req: Request) Decision;

/// Gate a tool call through the policy, then optional human/host approver.
pub fn authorize(policy: Policy, req: Request, approver_ctx: ?*anyopaque, approver: ?Approver) Decision {
    const base = policy.decide(req);
    return switch (base) {
        .allow, .deny => base,
        .ask => if (approver) |cb| cb(approver_ctx, req) else .deny,
    };
}

test "read_only allows reads and asks on shell" {
    const policy = Policy{ .mode = .read_only };
    try std.testing.expectEqual(Decision.allow, policy.decide(.{ .tool_name = "read_file", .arguments = "{}" }));
    try std.testing.expectEqual(Decision.ask, policy.decide(.{ .tool_name = "run_command", .arguments = "{}" }));
}

test "deny_all blocks everything unless allowlisted" {
    const policy = Policy{ .mode = .deny_all, .allowlist = &.{"read_file"} };
    try std.testing.expectEqual(Decision.allow, policy.decide(.{ .tool_name = "read_file", .arguments = "{}" }));
    try std.testing.expectEqual(Decision.deny, policy.decide(.{ .tool_name = "write_file", .arguments = "{}" }));
}

test "authorize uses approver on ask" {
    const policy = Policy{ .mode = .read_only };
    const allow_cb = struct {
        fn f(_: ?*anyopaque, _: Request) Decision {
            return .allow;
        }
    }.f;
    const decision = authorize(policy, .{ .tool_name = "run_command", .arguments = "{}" }, null, allow_cb);
    try std.testing.expectEqual(Decision.allow, decision);
    _ = log;
}
