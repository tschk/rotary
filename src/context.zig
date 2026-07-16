const std = @import("std");

const log = std.log.scoped(.context);

/// Project/global instruction file names searched in order (dot/codex/zero style).
pub const instruction_names = [_][]const u8{
    "AGENTS.md",
    "CLAUDE.md",
    "ROTARY.md",
    ".rotary/instructions.md",
};

pub const Loaded = struct {
    allocator: std.mem.Allocator,
    path: []const u8,
    content: []const u8,

    pub fn deinit(self: *Loaded) void {
        self.allocator.free(self.path);
        self.allocator.free(self.content);
    }
};

/// Load the first instruction file found under `root`, or null if none.
pub fn loadProjectInstructions(allocator: std.mem.Allocator, io: std.Io, root: []const u8) !?Loaded {
    for (instruction_names) |name| {
        const path = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ root, name });
        defer allocator.free(path);

        const file = std.Io.Dir.cwd().openFile(io, path, .{}) catch continue;
        defer file.close(io);

        var content_buf: std.Io.Writer.Allocating = .init(allocator);
        errdefer content_buf.deinit();
        var read_buf: [4096]u8 = undefined;
        var file_reader = file.readerStreaming(io, &read_buf);
        const reader = &file_reader.interface;
        while (true) {
            const n = reader.readSliceShort(&read_buf) catch break;
            if (n == 0) break;
            try content_buf.writer.writeAll(read_buf[0..n]);
        }

        const content = try content_buf.toOwnedSlice();
        log.info("loaded project instructions from {s} ({d} bytes)", .{ path, content.len });
        return .{
            .allocator = allocator,
            .path = try allocator.dupe(u8, path),
            .content = content,
        };
    }
    return null;
}

/// Merge system prompt + optional project instructions into one owned string.
pub fn composeSystemPrompt(allocator: std.mem.Allocator, base: ?[]const u8, project: ?[]const u8) !?[]const u8 {
    if (base == null and project == null) return null;
    if (base) |b| {
        if (project) |p| {
            return try std.fmt.allocPrint(allocator, "{s}\n\n# Project instructions\n\n{s}", .{ b, p });
        }
        return try allocator.dupe(u8, b);
    }
    return try std.fmt.allocPrint(allocator, "# Project instructions\n\n{s}", .{project.?});
}

/// Compact a long message history by keeping the first `keep_first` and last `keep_last`
/// messages and inserting a summary placeholder for the middle. Inspired by pi/codex
/// context compaction on overflow.
pub fn compactMessages(
    allocator: std.mem.Allocator,
    messages: []const providerMessage,
    keep_first: usize,
    keep_last: usize,
    summary: []const u8,
) ![]providerMessage {
    if (messages.len <= keep_first + keep_last) {
        const out = try allocator.alloc(providerMessage, messages.len);
        for (messages, 0..) |m, i| {
            out[i] = .{
                .role = m.role,
                .content = try allocator.dupe(u8, m.content),
            };
        }
        return out;
    }

    const count = keep_first + 1 + keep_last;
    const out = try allocator.alloc(providerMessage, count);
    errdefer {
        for (out[0..]) |m| allocator.free(m.content);
        allocator.free(out);
    }

    var idx: usize = 0;
    var i: usize = 0;
    while (i < keep_first) : (i += 1) {
        out[idx] = .{
            .role = messages[i].role,
            .content = try allocator.dupe(u8, messages[i].content),
        };
        idx += 1;
    }

    out[idx] = .{
        .role = .system,
        .content = try std.fmt.allocPrint(allocator, "[compacted {d} messages] {s}", .{ messages.len - keep_first - keep_last, summary }),
    };
    idx += 1;

    i = messages.len - keep_last;
    while (i < messages.len) : (i += 1) {
        out[idx] = .{
            .role = messages[i].role,
            .content = try allocator.dupe(u8, messages[i].content),
        };
        idx += 1;
    }
    return out;
}

const providerMessage = @import("provider.zig").Message;

test "compose system prompt merges sections" {
    const gpa = std.testing.allocator;
    const merged = try composeSystemPrompt(gpa, "base", "project rules");
    defer if (merged) |m| gpa.free(m);
    try std.testing.expect(merged != null);
    try std.testing.expect(std.mem.indexOf(u8, merged.?, "base") != null);
    try std.testing.expect(std.mem.indexOf(u8, merged.?, "project rules") != null);
}

test "compactMessages preserves edges" {
    const gpa = std.testing.allocator;
    const Role = @import("provider.zig").Role;
    var msgs = [_]providerMessage{
        .{ .role = .user, .content = "1" },
        .{ .role = .assistant, .content = "2" },
        .{ .role = .user, .content = "3" },
        .{ .role = .assistant, .content = "4" },
        .{ .role = .user, .content = "5" },
    };
    const out = try compactMessages(gpa, &msgs, 1, 1, "middle");
    defer {
        for (out) |m| gpa.free(m.content);
        gpa.free(out);
    }
    try std.testing.expectEqual(@as(usize, 3), out.len);
    try std.testing.expectEqualStrings("1", out[0].content);
    try std.testing.expect(std.mem.indexOf(u8, out[1].content, "middle") != null);
    try std.testing.expectEqualStrings("5", out[2].content);
    _ = Role;
}
