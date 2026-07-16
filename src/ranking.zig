const std = @import("std");

/// Proactive ranking inspired by Omi's proactive::list scoring.
pub const Category = enum {
    task,
    insight,
    calendar,
    briefing,
    other,
};

pub const Event = struct {
    id: []const u8,
    title: []const u8,
    category: Category,
    /// Higher is more urgent (0-100).
    score: i32,
    due_unix: ?i64 = null,
};

pub const RankOptions = struct {
    now_unix: i64,
    limit: usize = 6,
};

fn scoreTask(now: i64, due: ?i64, high_priority: bool) i32 {
    if (due) |d| {
        const delta = d - now;
        if (delta < 0) return 90;
        if (delta < 3600) return 90;
        if (delta < 86_400) return 85;
    }
    if (high_priority) return 80;
    return 50;
}

fn scoreInsight(unread: bool, confidence: f32) i32 {
    if (!unread) return 20;
    if (confidence >= 0.8) return 70;
    if (confidence >= 0.5) return 55;
    return 35;
}

fn scoreCalendar(now: i64, start: i64) i32 {
    const delta = start - now;
    if (delta < 0 and delta > -3600) return 100;
    if (delta >= 0 and delta < 3600) return 95;
    if (delta >= 0 and delta < 86_400) return 75;
    return 40;
}

pub fn makeTask(id: []const u8, title: []const u8, now: i64, due: ?i64, high_priority: bool) Event {
    return .{
        .id = id,
        .title = title,
        .category = .task,
        .score = scoreTask(now, due, high_priority),
        .due_unix = due,
    };
}

pub fn makeInsight(id: []const u8, title: []const u8, unread: bool, confidence: f32) Event {
    return .{
        .id = id,
        .title = title,
        .category = .insight,
        .score = scoreInsight(unread, confidence),
        .due_unix = null,
    };
}

pub fn makeCalendar(id: []const u8, title: []const u8, now: i64, start: i64) Event {
    return .{
        .id = id,
        .title = title,
        .category = .calendar,
        .score = scoreCalendar(now, start),
        .due_unix = start,
    };
}

/// Sort descending by score and truncate to `opts.limit`.
pub fn rank(allocator: std.mem.Allocator, events: []const Event, opts: RankOptions) ![]Event {
    _ = opts.now_unix;
    var list = try allocator.alloc(Event, events.len);
    @memcpy(list, events);
    std.mem.sort(Event, list, {}, struct {
        fn less(_: void, a: Event, b: Event) bool {
            return a.score > b.score;
        }
    }.less);
    if (list.len > opts.limit) {
        const trimmed = try allocator.alloc(Event, opts.limit);
        @memcpy(trimmed, list[0..opts.limit]);
        allocator.free(list);
        return trimmed;
    }
    return list;
}

test "overdue tasks beat low confidence insights" {
    const gpa = std.testing.allocator;
    const now: i64 = 1_700_000_000;
    const events = [_]Event{
        makeInsight("i1", "maybe", true, 0.4),
        makeTask("t1", "overdue", now, now - 100, false),
        makeCalendar("c1", "soon", now, now + 600),
    };
    const top = try rank(gpa, &events, .{ .now_unix = now, .limit = 2 });
    defer gpa.free(top);
    try std.testing.expectEqual(@as(usize, 2), top.len);
    try std.testing.expect(top[0].score >= top[1].score);
    try std.testing.expect(top[0].category == .calendar or top[0].category == .task);
}
