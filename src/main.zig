const std = @import("std");
const rotary = @import("rotary");

pub fn main(init: std.process.Init) !void {
    const io = init.io;
    const gpa = init.gpa;

    var stdout_buffer: [4096]u8 = undefined;
    var stdout_file_writer: std.Io.File.Writer = .init(.stdout(), io, &stdout_buffer);
    const stdout = &stdout_file_writer.interface;

    try rotary.printBanner(stdout);

    const args = try init.minimal.args.toSlice(init.arena.allocator());
    if (args.len > 1) {
        const cmd = args[1];
        if (std.mem.eql(u8, cmd, "agent")) {
            try runAgentDemo(gpa, io, stdout);
        } else if (std.mem.eql(u8, cmd, "provider")) {
            try runProviderDemo(gpa, stdout);
        } else if (std.mem.eql(u8, cmd, "session")) {
            try runSessionDemo(gpa, stdout);
        } else if (std.mem.eql(u8, cmd, "lsp")) {
            try runLspDemo(gpa, io, stdout);
        } else if (std.mem.eql(u8, cmd, "plugin")) {
            try runPluginDemo(gpa, io, stdout);
        } else if (std.mem.eql(u8, cmd, "plugin-pi")) {
            try runPluginPiDemo(gpa, io, stdout, args[2..]);
        } else if (std.mem.eql(u8, cmd, "acp")) {
            try runAcpDemo(gpa, io, stdout);
        } else if (std.mem.eql(u8, cmd, "serve")) {
            try runIpcServer(init, gpa, io, stdout, args[0], args[2..]);
        } else if (std.mem.eql(u8, cmd, "version")) {
            try stdout.print("{s}\n", .{rotary.version});
        } else {
            try printUsage(stdout);
        }
    } else {
        try printUsage(stdout);
    }

    try stdout.flush();
}

fn printUsage(stdout: *std.Io.Writer) !void {
    try stdout.print(
        \\Usage: rotary <command>
        \\
        \\Commands:
        \\  agent       Run the agent loop demo
        \\  provider    List built-in providers
        \\  session     Session CRUD / fork demo
        \\  lsp         List LSP language servers
        \\  plugin      Load pi-compatible extensions
        \\  plugin-pi   Run a specific pi extension path
        \\  acp         Spawn an ACP child agent
        \\  serve       Start the JSON-RPC IPC server
        \\  version     Print version
        \\
    , .{});
}

fn runAgentDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var arena_state = std.heap.ArenaAllocator.init(gpa);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var agent = rotary.Agent.init(arena, io);
    defer agent.deinit();

    var tools = rotary.ToolRegistry.init(arena);
    defer tools.deinit();
    rotary.tools.setIo(io);
    try rotary.tools.registerBuiltins(&tools);
    agent.setTools(&tools);

    try agent.subscribe(null, onEvent);
    try agent.prompt("Hello, rotary!");

    try stdout.print("Agent demo complete. tools={d}\n", .{tools.count()});
}

fn onEvent(ctx: ?*anyopaque, event: rotary.Event) void {
    _ = ctx;
    std.log.info("event: {}", .{event});
}

fn runProviderDemo(gpa: std.mem.Allocator, stdout: *std.Io.Writer) !void {
    var providers = rotary.ProviderRegistry.init(gpa);
    defer providers.deinit();

    try providers.add(.openai);
    try providers.add(.anthropic);
    try providers.add(.google);
    try providers.add(.local);

    try stdout.print("Providers: {d}\n", .{providers.count()});
}

fn runSessionDemo(gpa: std.mem.Allocator, stdout: *std.Io.Writer) !void {
    var s = try rotary.Session.init(gpa, "demo", "demo session");
    defer s.deinit();
    _ = try s.append(.user, "hello");
    _ = try s.append(.assistant, "hi from rotary");
    try stdout.print("Session {s} entries={d}\n", .{ s.id, s.entries.items.len });
}

fn runLspDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var manager = rotary.lsp.Manager.init(gpa, io);
    defer manager.deinit();
    try stdout.print("LSP manager ready\n", .{});
}

fn runPluginDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var registry = rotary.plugin.Registry.init(gpa, io);
    defer registry.deinit();
    try stdout.print("Plugin registry ready (pi-compatible Bun extensions)\n", .{});
}

fn runPluginPiDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer, args: []const []const u8) !void {
    _ = io;
    if (args.len == 0) {
        try stdout.print("Usage: rotary plugin-pi <extension.ts>\n", .{});
        return;
    }
    try stdout.print("Would load pi extension: {s}\n", .{args[0]});
    _ = gpa;
}

fn runAcpDemo(gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer) !void {
    var host = rotary.acp.Host.init(gpa, io);
    defer host.deinit();
    try stdout.print("ACP host ready\n", .{});
}

fn runIpcServer(init: std.process.Init, gpa: std.mem.Allocator, io: std.Io, stdout: *std.Io.Writer, argv0: []const u8, args: []const []const u8) !void {
    _ = argv0;
    const path = if (args.len > 0) args[0] else "/tmp/rotary.sock";

    var arena_state = std.heap.ArenaAllocator.init(gpa);
    defer arena_state.deinit();
    const arena = arena_state.allocator();

    var agent_instance = rotary.Agent.init(arena, io);
    defer agent_instance.deinit();

    var tool_registry = rotary.ToolRegistry.init(arena);
    defer tool_registry.deinit();
    rotary.tools.setIo(io);
    try rotary.tools.registerBuiltins(&tool_registry);
    agent_instance.setTools(&tool_registry);

    var plugin_registry = rotary.plugin.Registry.init(gpa, io);
    defer plugin_registry.deinit();

    var acp_host = rotary.acp.Host.init(gpa, io);
    defer acp_host.deinit();

    const data_dir = blk: {
        if (init.environ_map.get("ROTARY_HOME")) |home| break :blk home;
        break :blk ".rotary";
    };
    var session_store = rotary.session.Store.init(gpa, io, data_dir) catch |err| {
        try stdout.print("session store init failed: {}\n", .{err});
        return err;
    };
    defer session_store.deinit();

    var server = rotary.ipc.Server.init(gpa, io, path);
    defer server.deinit();
    server.attachAgent(&agent_instance);
    server.attachTools(&tool_registry);
    server.attachPlugins(&plugin_registry);
    server.attachSessionStore(&session_store);
    server.attachAcpHost(&acp_host);

    try stdout.print("rotary IPC server starting on {s}\n", .{path});
    try stdout.flush();
    try server.run();
}

pub const std_options: std.Options = .{
    .log_level = .info,
};
