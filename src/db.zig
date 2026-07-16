const std = @import("std");

const log = std.log.scoped(.db);

fn escapeJson(allocator: std.mem.Allocator, s: []const u8) ![]u8 {
    var count: usize = 0;
    for (s) |c| {
        count += switch (c) {
            '"', '\\' => 2,
            '\n', '\r', '\t' => 2,
            else => 1,
        };
    }
    var result = try allocator.alloc(u8, count);
    var i: usize = 0;
    for (s) |c| {
        switch (c) {
            '"' => {
                result[i] = '\\';
                result[i + 1] = '"';
                i += 2;
            },
            '\\' => {
                result[i] = '\\';
                result[i + 1] = '\\';
                i += 2;
            },
            '\n' => {
                result[i] = '\\';
                result[i + 1] = 'n';
                i += 2;
            },
            '\r' => {
                result[i] = '\\';
                result[i + 1] = 'r';
                i += 2;
            },
            '\t' => {
                result[i] = '\\';
                result[i + 1] = 't';
                i += 2;
            },
            else => {
                result[i] = c;
                i += 1;
            },
        }
    }
    return result;
}

fn valStr(v: std.json.Value, key: []const u8) ?[]const u8 {
    if (v != .object) return null;
    const entry = v.object.get(key) orelse return null;
    if (entry != .string) return null;
    return entry.string;
}

fn valInt(v: std.json.Value, key: []const u8) ?i64 {
    if (v != .object) return null;
    const entry = v.object.get(key) orelse return null;
    if (entry != .integer) return null;
    return entry.integer;
}

/// JSON-RPC client for the rotary-db (limbo) subprocess.
/// Protocol: {"id":N,"method":"execute|query|ping","params":{"sql":"..."}}
pub const Client = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    process: ?std.process.Child = null,
    read_buf: []u8 = &.{},
    next_id: u64 = 1,

    pub fn init(allocator: std.mem.Allocator, io: std.Io) Client {
        return .{ .allocator = allocator, .io = io };
    }

    pub fn deinit(self: *Client) void {
        self.stop();
    }

    pub fn start(self: *Client, db_path: []const u8, helper_path: []const u8) !void {
        self.process = try std.process.spawn(self.io, .{
            .argv = &.{ helper_path, db_path },
            .stdin = .pipe,
            .stdout = .pipe,
            .stderr = .inherit,
        });
        self.read_buf = try self.allocator.alloc(u8, 65536);
        log.info("db helper started: {s} {s}", .{ helper_path, db_path });
    }

    pub fn stop(self: *Client) void {
        if (self.process) |*p| {
            p.kill(self.io);
            self.process = null;
        }
        if (self.read_buf.len > 0) {
            self.allocator.free(self.read_buf);
            self.read_buf = &.{};
        }
    }

    fn sendRaw(self: *Client, json_line: []const u8) !u64 {
        const p = self.process orelse return error.DatabaseNotRunning;
        const file = p.stdin orelse return error.DatabaseNotRunning;
        const id = self.next_id;
        self.next_id += 1;

        var buf: [4096]u8 = undefined;
        var file_writer: std.Io.File.Writer = .init(file, self.io, &buf);
        const writer = &file_writer.interface;
        try writer.writeAll("{\"id\":");
        try writer.print("{d}", .{id});
        try writer.writeAll(",");
        try writer.writeAll(json_line);
        try writer.writeAll("\n");
        try writer.flush();
        return id;
    }

    fn readResponse(self: *Client, expected_id: u64) !std.json.Parsed(std.json.Value) {
        const p = self.process orelse return error.DatabaseNotRunning;
        const file = p.stdout orelse return error.DatabaseNotRunning;
        var reader = std.Io.File.reader(file, self.io, self.read_buf);
        const r = &reader.interface;
        const line = try r.takeDelimiter('\n') orelse return error.DatabaseClosed;
        const trimmed = std.mem.trim(u8, line, " \r\n");
        const parsed = try std.json.parseFromSlice(std.json.Value, self.allocator, trimmed, .{
            .ignore_unknown_fields = true,
            .allocate = .alloc_always,
        });
        errdefer parsed.deinit();
        const response = parsed.value;
        const rid = valInt(response, "id") orelse return error.InvalidResponse;
        if (rid != expected_id) return error.ResponseIdMismatch;
        return parsed;
    }

    pub fn execute(self: *Client, sql: []const u8) !u64 {
        const escaped = try escapeJson(self.allocator, sql);
        defer self.allocator.free(escaped);
        const params = try std.fmt.allocPrint(self.allocator, "\"method\":\"execute\",\"params\":{{\"sql\":\"{s}\"}}", .{escaped});
        defer self.allocator.free(params);
        const id = try self.sendRaw(params);
        const parsed = try self.readResponse(id);
        defer parsed.deinit();
        const resp = parsed.value;

        if (valStr(resp, "error")) |e| {
            if (e.len > 0) log.err("db error: {s}", .{e});
            return 0;
        }
        const result = resp.object.get("result") orelse return 0;
        return switch (result) {
            .integer => |n| @intCast(@max(n, 0)),
            .object => |obj| if (obj.get("rows_affected")) |rows| switch (rows) {
                .integer => |n| @intCast(@max(n, 0)),
                else => 0,
            } else 0,
            else => 0,
        };
    }

    pub fn executeMany(self: *Client, sql_list: []const []const u8) !u64 {
        for (sql_list) |sql| {
            _ = try self.execute(sql);
        }
        return 0;
    }

    pub const QueryResult = struct {
        allocator: std.mem.Allocator,
        columns: [][]const u8,
        rows: [][]const u8,

        pub fn deinit(self: *QueryResult) void {
            for (self.columns) |column| self.allocator.free(column);
            for (self.rows) |row| self.allocator.free(row);
            if (self.columns.len > 0) self.allocator.free(self.columns);
            if (self.rows.len > 0) self.allocator.free(self.rows);
        }
    };

    pub fn query(self: *Client, sql: []const u8) !QueryResult {
        const escaped = try escapeJson(self.allocator, sql);
        defer self.allocator.free(escaped);
        const params = try std.fmt.allocPrint(self.allocator, "\"method\":\"query\",\"params\":{{\"sql\":\"{s}\"}}", .{escaped});
        defer self.allocator.free(params);
        const id = try self.sendRaw(params);
        const parsed = try self.readResponse(id);
        defer parsed.deinit();
        const resp = parsed.value;

        const result = resp.object.get("result") orelse return QueryResult{ .allocator = self.allocator, .columns = &.{}, .rows = &.{} };

        var columns: std.ArrayList([]const u8) = .empty;
        defer columns.deinit(self.allocator);
        errdefer for (columns.items) |column| self.allocator.free(column);
        if (result == .object) {
            if (result.object.get("columns")) |cols_v| {
                if (cols_v == .array) {
                    for (cols_v.array.items) |col| {
                        if (col == .string) {
                            try columns.append(self.allocator, try self.allocator.dupe(u8, col.string));
                        }
                    }
                }
            }
        }

        var rows: std.ArrayList([]const u8) = .empty;
        defer rows.deinit(self.allocator);
        errdefer for (rows.items) |row| self.allocator.free(row);
        if (result == .object) {
            if (result.object.get("rows")) |rows_v| {
                if (rows_v == .array) {
                    for (rows_v.array.items) |row| {
                        var buf: std.Io.Writer.Allocating = .init(self.allocator);
                        defer buf.deinit();
                        try std.json.Stringify.value(row, .{}, &buf.writer);
                        try rows.append(self.allocator, try self.allocator.dupe(u8, buf.written()));
                    }
                }
            }
        }

        return QueryResult{
            .allocator = self.allocator,
            .columns = try columns.toOwnedSlice(self.allocator),
            .rows = try rows.toOwnedSlice(self.allocator),
        };
    }

    pub fn ping(self: *Client) !bool {
        const id = try self.sendRaw("\"method\":\"ping\",\"params\":{}");
        const parsed = try self.readResponse(id);
        defer parsed.deinit();
        const resp = parsed.value;
        return resp.object.get("result") != null;
    }

    pub fn migrate(self: *Client) !void {
        _ = try self.sendRaw("\"method\":\"migrate\",\"params\":{}");
    }
};

test "query result frees owned fields" {
    const gpa = std.testing.allocator;
    var columns: std.ArrayList([]const u8) = .empty;
    try columns.append(gpa, try gpa.dupe(u8, "name"));
    var rows: std.ArrayList([]const u8) = .empty;
    try rows.append(gpa, try gpa.dupe(u8, "[\"session\"]"));

    var result = Client.QueryResult{
        .allocator = gpa,
        .columns = try columns.toOwnedSlice(gpa),
        .rows = try rows.toOwnedSlice(gpa),
    };
    result.deinit();
}
