const std = @import("std");
const agent = @import("agent.zig");
const lsp = @import("lsp.zig");

const log = std.log.scoped(.tools);

var global_io: ?std.Io = null;
var global_lsp: ?*lsp.Manager = null;

pub fn setIo(io: std.Io) void {
    global_io = io;
}

pub fn setLsp(manager: *lsp.Manager) void {
    global_lsp = manager;
}

fn io_global() std.Io {
    return global_io orelse std.Io.Threaded.global_single_threaded.io();
}

pub fn registerBuiltins(registry: *agent.ToolRegistry) !void {
    try registry.register(.{
        .name = "read_file",
        .description = "Read the contents of a file at the given path.",
        .parameters_json =
        \\{"type":"object","properties":{"path":{"type":"string","description":"Absolute or relative file path"}},"required":["path"]}
        ,
        .execute = readFileExecute,
    });

    try registry.register(.{
        .name = "write_file",
        .description = "Write content to a file, creating or overwriting it.",
        .parameters_json =
        \\{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}
        ,
        .execute = writeFileExecute,
    });

    try registry.register(.{
        .name = "list_dir",
        .description = "List entries in a directory.",
        .parameters_json =
        \\{"type":"object","properties":{"path":{"type":"string","description":"Directory path"}},"required":["path"]}
        ,
        .execute = listDirExecute,
    });

    try registry.register(.{
        .name = "run_command",
        .description = "Run a shell command and return stdout/stderr.",
        .parameters_json =
        \\{"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"}},"required":["command"]}
        ,
        .execute = runCommandExecute,
    });

    try registry.register(.{
        .name = "find_files",
        .description = "Search for files by name pattern. Uses fff if available, then fd/fdfind, then find.",
        .parameters_json =
        \\{"type":"object","properties":{"pattern":{"type":"string","description":"File name pattern to search for"},"path":{"type":"string","description":"Directory to search in (default: cwd)"}},"required":["pattern"]}
        ,
        .execute = findFilesExecute,
    });

    try registry.register(.{
        .name = "spawn_agent",
        .description = "Spawn a child agent subprocess and send it a task. Main agent commands subagents hierarchically.",
        .parameters_json =
        \\{"type":"object","properties":{"command":{"type":"string","description":"Agent command to spawn"},"task":{"type":"string","description":"Task/prompt to send to subagent"}},"required":["command","task"]}
        ,
        .execute = spawnAgentExecute,
    });

    try registry.register(.{
        .name = "code_intel",
        .description = "Query the language server for diagnostics, hover information, or symbol definitions.",
        .parameters_json =
        \\{"type":"object","properties":{"action":{"type":"string","enum":["diagnostics","hover","definition"]},"language":{"type":"string"},"root_uri":{"type":"string"},"uri":{"type":"string"},"text":{"type":"string"},"version":{"type":"integer"},"line":{"type":"integer"},"character":{"type":"integer"}},"required":["action","language","root_uri","uri","text"]}
        ,
        .execute = codeIntelExecute,
    });
}

const CodeIntelArgs = struct {
    action: enum { diagnostics, hover, definition },
    language: []const u8,
    root_uri: []const u8,
    uri: []const u8,
    text: []const u8,
    version: i64 = 1,
    line: u32 = 0,
    character: u32 = 0,
};

fn codeIntelError(allocator: std.mem.Allocator, message: []const u8) !agent.ToolResult {
    return .{
        .id = "code_intel",
        .content = try std.fmt.allocPrint(allocator, "Language-server request failed: {s}", .{message}),
        .is_error = true,
    };
}

fn codeIntelExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !agent.ToolResult {
    _ = ctx;
    const manager = global_lsp orelse return codeIntelError(allocator, "LspUnavailable");
    const parsed = std.json.parseFromSlice(CodeIntelArgs, allocator, arguments, .{
        .ignore_unknown_fields = true,
    }) catch |err| return codeIntelError(allocator, @errorName(err));
    defer parsed.deinit();
    const args = parsed.value;
    const client = manager.startForLanguage(args.language) catch |err| return codeIntelError(allocator, @errorName(err));
    if (!client.initialized) client.initialize(args.root_uri) catch |err| return codeIntelError(allocator, @errorName(err));
    client.didOpen(args.uri, args.language, args.text, args.version) catch |err| return codeIntelError(allocator, @errorName(err));

    var result: std.Io.Writer.Allocating = .init(allocator);
    defer result.deinit();
    switch (args.action) {
        .diagnostics => {
            _ = client.hover(args.uri, args.line, args.character) catch |err| return codeIntelError(allocator, @errorName(err));
            for (client.diagnostics(args.uri)) |diagnostic| {
                try result.writer.print("{s}:{d}:{d}: {s}\n", .{ diagnostic.uri, diagnostic.line, diagnostic.character, diagnostic.message });
            }
            if (result.written().len == 0) try result.writer.writeAll("No diagnostics.");
        },
        .hover => {
            if (try client.hover(args.uri, args.line, args.character)) |hover| {
                defer hover.deinit(allocator);
                try result.writer.writeAll(hover.contents);
            } else {
                try result.writer.writeAll("No hover information.");
            }
        },
        .definition => {
            const locations = client.definition(args.uri, args.line, args.character) catch |err| return codeIntelError(allocator, @errorName(err));
            defer client.freeLocations(locations);
            for (locations) |location| {
                try result.writer.print("{s}:{d}:{d}-{d}:{d}\n", .{ location.uri, location.start_line, location.start_character, location.end_line, location.end_character });
            }
            if (locations.len == 0) try result.writer.writeAll("No definition found.");
        },
    }
    return .{
        .id = "code_intel",
        .content = try allocator.dupe(u8, result.written()),
        .is_error = false,
    };
}

fn parsePathArg(allocator: std.mem.Allocator, arguments: []const u8) ![]const u8 {
    const parsed = try std.json.parseFromSlice(PathArgs, allocator, arguments, .{
        .ignore_unknown_fields = true,
    });
    defer parsed.deinit();
    return try allocator.dupe(u8, parsed.value.path);
}

const PathArgs = struct {
    path: []const u8,
};

fn readFileExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !agent.ToolResult {
    _ = ctx;
    const path = try parsePathArg(allocator, arguments);
    defer allocator.free(path);

    const io = io_global();
    const file = std.Io.Dir.cwd().openFile(io, path, .{}) catch |err| {
        return .{
            .id = "read_file",
            .content = try std.fmt.allocPrint(allocator, "Failed to open {s}: {}", .{ path, err }),
            .is_error = true,
        };
    };
    defer file.close(io);

    var read_buf: [4096]u8 = undefined;
    var content_buf: std.Io.Writer.Allocating = .init(allocator);
    const out = &content_buf.writer;
    defer content_buf.deinit();

    var file_reader = file.readerStreaming(io, &read_buf);
    const reader = &file_reader.interface;
    while (true) {
        const n = reader.readSliceShort(&read_buf) catch break;
        if (n == 0) break;
        try out.writeAll(read_buf[0..n]);
    }

    return .{
        .id = "read_file",
        .content = try allocator.dupe(u8, content_buf.written()),
        .is_error = false,
    };
}

fn writeFileExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !agent.ToolResult {
    _ = ctx;
    const parsed = try std.json.parseFromSlice(WriteFileArgs, allocator, arguments, .{
        .ignore_unknown_fields = true,
    });
    defer parsed.deinit();

    const path = parsed.value.path;
    const content = parsed.value.content;
    const io = io_global();

    const file = std.Io.Dir.cwd().createFile(io, path, .{ .truncate = true }) catch |err| {
        return .{
            .id = "write_file",
            .content = try std.fmt.allocPrint(allocator, "Failed to create {s}: {}", .{ path, err }),
            .is_error = true,
        };
    };
    defer file.close(io);

    file.writeStreamingAll(io, content) catch |err| {
        return .{
            .id = "write_file",
            .content = try std.fmt.allocPrint(allocator, "Failed to write {s}: {}", .{ path, err }),
            .is_error = true,
        };
    };

    return .{
        .id = "write_file",
        .content = try std.fmt.allocPrint(allocator, "Wrote {d} bytes to {s}", .{ content.len, path }),
        .is_error = false,
    };
}

const WriteFileArgs = struct {
    path: []const u8,
    content: []const u8,
};

fn listDirExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !agent.ToolResult {
    _ = ctx;
    const path = try parsePathArg(allocator, arguments);
    defer allocator.free(path);

    const io = io_global();
    var dir = std.Io.Dir.cwd().openDir(io, path, .{ .iterate = true }) catch |err| {
        return .{
            .id = "list_dir",
            .content = try std.fmt.allocPrint(allocator, "Failed to open dir {s}: {}", .{ path, err }),
            .is_error = true,
        };
    };
    defer dir.close(io);

    var buf: std.Io.Writer.Allocating = .init(allocator);
    const out = &buf.writer;

    var iter = dir.iterate();
    while (try iter.next(io)) |entry| {
        const type_str = switch (entry.kind) {
            .file => "F",
            .directory => "D",
            .sym_link => "L",
            else => "?",
        };
        try out.print("{s} {s}\n", .{ type_str, entry.name });
    }

    return .{
        .id = "list_dir",
        .content = try allocator.dupe(u8, buf.written()),
        .is_error = false,
    };
}

fn runCommandExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !agent.ToolResult {
    _ = ctx;
    const parsed = try std.json.parseFromSlice(RunCommandArgs, allocator, arguments, .{
        .ignore_unknown_fields = true,
    });
    defer parsed.deinit();

    const command = parsed.value.command;
    const cwd = parsed.value.cwd;
    const io = io_global();

    const cwd_opt: std.process.Child.Cwd = if (cwd) |c| .{ .path = c } else .inherit;

    const argv: []const []const u8 = switch (@import("builtin").os.tag) {
        .windows => &.{ "cmd.exe", "/c", command },
        else => &.{ "sh", "-c", command },
    };

    var child = std.process.spawn(io, .{
        .argv = argv,
        .cwd = cwd_opt,
        .stdout = .pipe,
        .stderr = .pipe,
    }) catch |err| {
        return .{
            .id = "run_command",
            .content = try std.fmt.allocPrint(allocator, "Failed to spawn: {}", .{err}),
            .is_error = true,
        };
    };

    var stdout_buf: std.Io.Writer.Allocating = .init(allocator);
    const out = &stdout_buf.writer;
    defer stdout_buf.deinit();

    if (child.stdout) |stdout_file| {
        var read_buf: [4096]u8 = undefined;
        var file_reader = stdout_file.readerStreaming(io, &read_buf);
        const reader = &file_reader.interface;
        while (true) {
            const n = reader.readSliceShort(&read_buf) catch break;
            if (n == 0) break;
            try out.writeAll(read_buf[0..n]);
        }
    }

    const term = child.wait(io) catch |err| {
        return .{
            .id = "run_command",
            .content = try std.fmt.allocPrint(allocator, "Wait failed: {}", .{err}),
            .is_error = true,
        };
    };

    const exit_code: i32 = switch (term) {
        .exited => |code| @intCast(code),
        else => -1,
    };

    var result_buf: std.Io.Writer.Allocating = .init(allocator);
    const result_out = &result_buf.writer;
    try result_out.print("exit: {d}\n{s}", .{ exit_code, stdout_buf.written() });

    return .{
        .id = "run_command",
        .content = try allocator.dupe(u8, result_buf.written()),
        .is_error = exit_code != 0,
    };
}

const FindFilesArgs = struct {
    pattern: []const u8,
    path: ?[]const u8 = null,
};

fn tryFind(allocator: std.mem.Allocator, io: std.Io, cmd: []const u8, pattern: []const u8, search_dir: []const u8) !?[]u8 {
    const shell_cmd = blk: {
        if (std.mem.eql(u8, cmd, "fff")) {
            break :blk try std.fmt.allocPrint(allocator, "fff find '{s}' --path '{s}' 2>/dev/null", .{ pattern, search_dir });
        } else if (std.mem.eql(u8, cmd, "fd")) {
            break :blk try std.fmt.allocPrint(allocator, "fd '{s}' '{s}' 2>/dev/null", .{ pattern, search_dir });
        } else if (std.mem.eql(u8, cmd, "fdfind")) {
            break :blk try std.fmt.allocPrint(allocator, "fdfind '{s}' '{s}' 2>/dev/null", .{ pattern, search_dir });
        } else {
            break :blk try std.fmt.allocPrint(allocator, "find '{s}' -name '{s}' 2>/dev/null | head -100", .{ search_dir, pattern });
        }
    };
    defer allocator.free(shell_cmd);

    var child = std.process.spawn(io, .{
        .argv = &.{ "sh", "-c", shell_cmd },
        .stdout = .pipe,
        .stderr = .pipe,
    }) catch return null;

    var stdout_buf: std.Io.Writer.Allocating = .init(allocator);
    const out = &stdout_buf.writer;
    defer stdout_buf.deinit();

    if (child.stdout) |stdout_file| {
        var read_buf: [4096]u8 = undefined;
        var file_reader = stdout_file.readerStreaming(io, &read_buf);
        const reader = &file_reader.interface;
        while (true) {
            const n = reader.readSliceShort(&read_buf) catch break;
            if (n == 0) break;
            try out.writeAll(read_buf[0..n]);
        }
    }

    const term = child.wait(io) catch return null;
    const exited = switch (term) {
        .exited => true,
        else => false,
    };

    const output = stdout_buf.written();
    if (exited and output.len > 0) {
        return try allocator.dupe(u8, output);
    }
    return null;
}

fn findFilesExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !agent.ToolResult {
    _ = ctx;
    const parsed = try std.json.parseFromSlice(FindFilesArgs, allocator, arguments, .{
        .ignore_unknown_fields = true,
    });
    defer parsed.deinit();

    const pattern = parsed.value.pattern;
    const search_dir = parsed.value.path orelse ".";
    const io = io_global();

    const cmds = [_][]const u8{ "fff", "fd", "fdfind", "find" };
    for (cmds) |cmd| {
        if (try tryFind(allocator, io, cmd, pattern, search_dir)) |result| {
            return .{
                .id = "find_files",
                .content = result,
                .is_error = false,
            };
        }
    }

    return .{
        .id = "find_files",
        .content = try allocator.dupe(u8, "No results found with fff, fd, or find"),
        .is_error = false,
    };
}

const SpawnAgentArgs = struct {
    command: []const u8,
    task: []const u8,
};

fn spawnAgentExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !agent.ToolResult {
    _ = ctx;
    const parsed = try std.json.parseFromSlice(SpawnAgentArgs, allocator, arguments, .{
        .ignore_unknown_fields = true,
    });
    defer parsed.deinit();

    const command = parsed.value.command;
    const task = parsed.value.task;

    const io = io_global();
    const acp = @import("acp.zig");

    var host = acp.Host.init(allocator, io);
    defer host.deinit();

    const agent_proc = host.spawn("subagent", command, &.{task}) catch |err| {
        return .{
            .id = "spawn_agent",
            .content = try std.fmt.allocPrint(allocator, "Failed to spawn agent '{s}': {}", .{ command, err }),
            .is_error = true,
        };
    };

    agent_proc.initialize("{\"capabilities\":{}}") catch |err| {
        return .{
            .id = "spawn_agent",
            .content = try std.fmt.allocPrint(allocator, "Agent '{s}' initialization failed: {}", .{ command, err }),
            .is_error = true,
        };
    };

    var read_buf: [4096]u8 = undefined;
    const response = agent_proc.promptAndWait(task, &read_buf) catch |err| {
        return .{
            .id = "spawn_agent",
            .content = try std.fmt.allocPrint(allocator, "Agent '{s}' failed: {}", .{ command, err }),
            .is_error = true,
        };
    };

    const result = try std.fmt.allocPrint(allocator, "Agent '{s}' result:\n{s}", .{ command, response });
    return .{
        .id = "spawn_agent",
        .content = result,
        .is_error = false,
    };
}

const RunCommandArgs = struct {
    command: []const u8,
    cwd: ?[]const u8 = null,
};

test "builtins include code intelligence" {
    const gpa = std.testing.allocator;
    var registry = agent.ToolRegistry.init(gpa);
    defer registry.deinit();
    try registerBuiltins(&registry);
    try std.testing.expect(registry.get("code_intel") != null);
}
