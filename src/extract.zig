const std = @import("std");

const log = std.log.scoped(.extract);

/// Structured extraction inspired by Omi knowledge/proactive extraction contracts.
pub const Kind = enum {
    task,
    memory,
    goal,
    decision,
    action,
    insight,
    none,

    pub fn fromString(s: []const u8) Kind {
        if (std.ascii.eqlIgnoreCase(s, "task")) return .task;
        if (std.ascii.eqlIgnoreCase(s, "memory")) return .memory;
        if (std.ascii.eqlIgnoreCase(s, "goal")) return .goal;
        if (std.ascii.eqlIgnoreCase(s, "decision")) return .decision;
        if (std.ascii.eqlIgnoreCase(s, "action")) return .action;
        if (std.ascii.eqlIgnoreCase(s, "insight")) return .insight;
        return .none;
    }
};

pub const Item = struct {
    kind: Kind,
    text: []const u8,
};

/// Extract fenced or bare JSON object/array payload from model output.
pub fn extractJsonBlock(response: []const u8) []const u8 {
    const trimmed = std.mem.trim(u8, response, " \t\r\n");
    if (std.mem.indexOf(u8, trimmed, "```")) |start| {
        var rest = trimmed[start + 3 ..];
        if (std.mem.startsWith(u8, rest, "json")) {
            rest = rest[4..];
        }
        rest = std.mem.trimLeft(u8, rest, " \t\r\n");
        if (std.mem.indexOf(u8, rest, "```")) |end| {
            return std.mem.trim(u8, rest[0..end], " \t\r\n");
        }
    }
    return trimmed;
}

/// Parse `{"kind":"...","text":"..."}` when kind matches `expected`.
pub fn parseOne(allocator: std.mem.Allocator, expected: Kind, response: []const u8) !?Item {
    const payload = extractJsonBlock(response);
    const parsed = std.json.parseFromSlice(std.json.Value, allocator, payload, .{}) catch return null;
    defer parsed.deinit();
    const obj = switch (parsed.value) {
        .object => |o| o,
        else => return null,
    };
    const kind_v = obj.get("kind") orelse return null;
    const text_v = obj.get("text") orelse return null;
    const kind_s = switch (kind_v) {
        .string => |s| s,
        else => return null,
    };
    const text_s = switch (text_v) {
        .string => |s| s,
        else => return null,
    };
    const kind = Kind.fromString(kind_s);
    if (kind != expected or kind == .none) return null;
    const text = std.mem.trim(u8, text_s, " \t\r\n");
    if (text.len == 0 or text.len > 240) return null;
    return .{
        .kind = kind,
        .text = try allocator.dupe(u8, text),
    };
}

/// Parse a JSON array of `{kind,text}` objects (task/memory/goal/decision/action/insight).
pub fn parseMany(allocator: std.mem.Allocator, response: []const u8) ![]Item {
    const payload = extractJsonBlock(response);
    const parsed = std.json.parseFromSlice(std.json.Value, allocator, payload, .{}) catch return try allocator.alloc(Item, 0);
    defer parsed.deinit();
    const arr = switch (parsed.value) {
        .array => |a| a,
        else => return try allocator.alloc(Item, 0),
    };

    var list: std.ArrayList(Item) = .empty;
    errdefer {
        for (list.items) |it| allocator.free(it.text);
        list.deinit(allocator);
    }

    for (arr.items) |value| {
        const obj = switch (value) {
            .object => |o| o,
            else => continue,
        };
        const kind_v = obj.get("kind") orelse continue;
        const text_v = obj.get("text") orelse continue;
        const kind_s = switch (kind_v) {
            .string => |s| s,
            else => continue,
        };
        const text_s = switch (text_v) {
            .string => |s| s,
            else => continue,
        };
        const kind = Kind.fromString(kind_s);
        if (kind == .none) continue;
        const text = std.mem.trim(u8, text_s, " \t\r\n");
        if (text.len == 0 or text.len > 240) continue;
        try list.append(allocator, .{
            .kind = kind,
            .text = try allocator.dupe(u8, text),
        });
    }
    return try list.toOwnedSlice(allocator);
}

pub fn singlePrompt(name: []const u8, note: []const u8, allocator: std.mem.Allocator) ![]const u8 {
    return try std.fmt.allocPrint(allocator,
        \\Extract one explicit {s} from this note if present. Return exactly JSON {{"kind":"{s}","text":"..."}} only when the requested kind is explicit; otherwise return {{"kind":"none","text":""}}. Note: {s}
    , .{ name, name, note });
}

pub fn multiPrompt(note: []const u8, allocator: std.mem.Allocator) ![]const u8 {
    return try std.fmt.allocPrint(allocator,
        \\Extract up to one explicit task, memory, and goal from this note. Return exactly a JSON array of objects with kind task|memory|goal and a concise text field. Return [] when none are explicit. Note: {s}
    , .{note});
}

test "extractJsonBlock strips fences" {
    const raw =
        \\```json
        \\{"kind":"task","text":"ship rotary"}
        \\```
    ;
    const block = extractJsonBlock(raw);
    try std.testing.expect(std.mem.indexOf(u8, block, "\"task\"") != null);
}

test "parseOne matches expected kind" {
    const gpa = std.testing.allocator;
    const item = try parseOne(gpa, .task, "{\"kind\":\"task\",\"text\":\"ship it\"}");
    try std.testing.expect(item != null);
    defer gpa.free(item.?.text);
    try std.testing.expectEqualStrings("ship it", item.?.text);
    _ = log;
}
