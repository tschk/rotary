const std = @import("std");

/// Slash commands inspired by Omi chat-commands, Crush, OpenClaude, and Zero.
pub const Command = union(enum) {
    help,
    model: []const u8,
    clear,
    compact,
    permissions: ?[]const u8,
    tools,
    session_new: ?[]const u8,
    unknown: []const u8,
};

pub fn parse(input: []const u8) ?Command {
    const text = std.mem.trim(u8, input, " \t\r\n");
    if (text.len == 0 or text[0] != '/') return null;

    const body = std.mem.trim(u8, text[1..], " \t");
    if (body.len == 0) return .{ .help = {} };

    var it = std.mem.tokenizeAny(u8, body, " \t");
    const name = it.next() orelse return .{ .help = {} };
    const rest = std.mem.trim(u8, it.rest(), " \t");

    if (std.ascii.eqlIgnoreCase(name, "help") or std.mem.eql(u8, name, "?")) {
        return .{ .help = {} };
    }
    if (std.ascii.eqlIgnoreCase(name, "model")) {
        return .{ .model = rest };
    }
    if (std.ascii.eqlIgnoreCase(name, "clear")) {
        return .{ .clear = {} };
    }
    if (std.ascii.eqlIgnoreCase(name, "compact")) {
        return .{ .compact = {} };
    }
    if (std.ascii.eqlIgnoreCase(name, "permissions") or std.ascii.eqlIgnoreCase(name, "perms")) {
        return .{ .permissions = if (rest.len == 0) null else rest };
    }
    if (std.ascii.eqlIgnoreCase(name, "tools")) {
        return .{ .tools = {} };
    }
    if (std.ascii.eqlIgnoreCase(name, "new") or std.ascii.eqlIgnoreCase(name, "session")) {
        return .{ .session_new = if (rest.len == 0) null else rest };
    }
    return .{ .unknown = name };
}

pub fn helpText() []const u8 {
    return 
    \\/help                 Show this help
    \\/model [name]         Show or switch model
    \\/clear                Clear conversation messages
    \\/compact              Compact long context
    \\/permissions [mode]   full_access | read_only | workspace_write | deny_all
    \\/tools                List registered tools
    \\/new [name]           Start a new session
    ;
}

test "parse model and help" {
    const m = parse("/model gpt-4o").?;
    try std.testing.expect(m == .model);
    try std.testing.expectEqualStrings("gpt-4o", m.model);

    try std.testing.expect(parse("/help").? == .help);
    try std.testing.expect(parse("not a command") == null);
}

test "parse permissions and clear" {
    try std.testing.expect(parse("/clear").? == .clear);
    const p = parse("/permissions read_only").?;
    try std.testing.expect(p == .permissions);
    try std.testing.expectEqualStrings("read_only", p.permissions.?);
}
