const std = @import("std");
const provider = @import("provider.zig");

const log = std.log.scoped(.config);

pub const Config = struct {
    default_provider: ?[]const u8 = null,
    default_model: ?[]const u8 = null,
    api_keys: std.StringHashMap([]const u8),
    system_prompt: ?[]const u8 = null,
    data_dir: []const u8,

    pub fn init(allocator: std.mem.Allocator, data_dir: []const u8) Config {
        return .{
            .api_keys = std.StringHashMap([]const u8).init(allocator),
            .data_dir = data_dir,
        };
    }

    pub fn deinit(self: *Config) void {
        self.api_keys.deinit();
    }

    pub fn getApiKey(self: *const Config, provider_id: provider.ProviderId) ?[]const u8 {
        const key_name = @tagName(provider_id);
        if (self.api_keys.get(key_name)) |key| return key;
        return null;
    }

    pub fn getProviderConfig(self: *const Config) ?provider.Provider {
        const id = self.default_provider orelse return null;
        const pid = parseProviderId(id) orelse return null;
        const api_key = self.getApiKey(pid);
        const model = self.default_model orelse provider.ProviderId.defaultModel(pid);
        return .{
            .id = pid,
            .base_url = provider.ProviderId.defaultUrl(pid),
            .api_key = api_key,
            .default_model = model,
        };
    }
};

pub fn parseProviderId(s: []const u8) ?provider.ProviderId {
    if (std.mem.eql(u8, s, "openai")) return .openai;
    if (std.mem.eql(u8, s, "anthropic")) return .anthropic;
    if (std.mem.eql(u8, s, "google")) return .google;
    if (std.mem.eql(u8, s, "local")) return .local;
    return null;
}

pub fn load(allocator: std.mem.Allocator, io: std.Io, data_dir: []const u8, environ_map: *const std.process.Environ.Map) !Config {
    var config = Config.init(allocator, data_dir);

    const config_path = try std.fmt.allocPrint(allocator, "{s}/config.json", .{data_dir});
    defer allocator.free(config_path);

    const file = std.Io.Dir.cwd().openFile(io, config_path, .{}) catch {
        log.info("no config file at {s}, using defaults + env vars", .{config_path});
        try loadFromEnv(allocator, &config, environ_map);
        return config;
    };
    defer file.close(io);

    var read_buf: [4096]u8 = undefined;
    var content_buf: std.Io.Writer.Allocating = .init(allocator);
    const content_writer = &content_buf.writer;
    defer content_buf.deinit();

    var file_reader = file.readerStreaming(io, &read_buf);
    const reader = &file_reader.interface;
    while (true) {
        const n = reader.readSliceShort(&read_buf) catch break;
        if (n == 0) break;
        try content_writer.writeAll(read_buf[0..n]);
    }

    const content = content_buf.written();
    defer allocator.free(content);

    const parsed = std.json.parseFromSlice(JsonConfig, allocator, content, .{
        .ignore_unknown_fields = true,
    }) catch |err| {
        log.warn("failed to parse config: {}, using env vars", .{err});
        try loadFromEnv(allocator, &config, environ_map);
        return config;
    };
    defer parsed.deinit();

    const jc = parsed.value;
    if (jc.default_provider) |dp| {
        config.default_provider = try allocator.dupe(u8, dp);
    }
    if (jc.default_model) |dm| {
        config.default_model = try allocator.dupe(u8, dm);
    }
    if (jc.system_prompt) |sp| {
        config.system_prompt = try allocator.dupe(u8, sp);
    }

    if (jc.api_keys) |keys| {
        switch (keys) {
            .object => |obj| {
                var it = obj.iterator();
                while (it.next()) |entry| {
                    switch (entry.value_ptr.*) {
                        .string => |s| {
                            try config.api_keys.put(
                                try allocator.dupe(u8, entry.key_ptr.*),
                                try allocator.dupe(u8, s),
                            );
                        },
                        else => {},
                    }
                }
            },
            else => {},
        }
    }

    try loadFromEnv(allocator, &config, environ_map);

    if (config.default_provider == null) {
        if (config.api_keys.get("openai") != null) {
            config.default_provider = "openai";
        } else if (config.api_keys.get("anthropic") != null) {
            config.default_provider = "anthropic";
        } else if (config.api_keys.get("google") != null) {
            config.default_provider = "google";
        }
    }

    log.info("config loaded: provider={s}, model={s}", .{
        config.default_provider orelse "none",
        config.default_model orelse "default",
    });

    return config;
}

fn loadFromEnv(allocator: std.mem.Allocator, config: *Config, environ_map: *const std.process.Environ.Map) !void {
    const env_keys = [_]struct { env: []const u8, provider: []const u8 }{
        .{ .env = "OPENAI_API_KEY", .provider = "openai" },
        .{ .env = "ANTHROPIC_API_KEY", .provider = "anthropic" },
        .{ .env = "GOOGLE_API_KEY", .provider = "google" },
        .{ .env = "ROTARY_API_KEY", .provider = "openai" },
    };

    for (env_keys) |ek| {
        if (environ_map.get(ek.env)) |key| {
            if (key.len > 0 and !config.api_keys.contains(ek.provider)) {
                try config.api_keys.put(
                    try allocator.dupe(u8, ek.provider),
                    try allocator.dupe(u8, key),
                );
                log.info("loaded {s} from env", .{ek.env});
            }
        }
    }

    if (environ_map.get("ROTARY_PROVIDER")) |p| {
        if (parseProviderId(p) != null and config.default_provider == null) {
            config.default_provider = try allocator.dupe(u8, p);
        }
    }

    if (environ_map.get("ROTARY_MODEL")) |m| {
        if (config.default_model == null) {
            config.default_model = try allocator.dupe(u8, m);
        }
    }
}

const JsonConfig = struct {
    default_provider: ?[]const u8 = null,
    default_model: ?[]const u8 = null,
    system_prompt: ?[]const u8 = null,
    api_keys: ?std.json.Value = null,
};

pub fn writeDefault(allocator: std.mem.Allocator, io: std.Io, data_dir: []const u8) !void {
    const config_path = try std.fmt.allocPrint(allocator, "{s}/config.json", .{data_dir});
    defer allocator.free(config_path);

    const file = std.Io.Dir.cwd().createFile(io, config_path, .{}) catch return;
    defer file.close(io);

    const default_config =
        \\{
        \\  "default_provider": "openai",
        \\  "default_model": "gpt-4o",
        \\  "api_keys": {
        \\    "openai": "sk-your-key-here"
        \\  },
        \\  "system_prompt": "You are a helpful coding assistant."
        \\}
    ;

    try file.writeStreamingAll(io, default_config);
    log.info("wrote default config to {s}", .{config_path});
}

test "config init/deinit" {
    const gpa = std.testing.allocator;
    var config = Config.init(gpa, ".rotary-test");
    defer config.deinit();
    try std.testing.expectEqualStrings(".rotary-test", config.data_dir);
}

test "parse provider id" {
    try std.testing.expectEqual(provider.ProviderId.openai, parseProviderId("openai").?);
    try std.testing.expectEqual(provider.ProviderId.anthropic, parseProviderId("anthropic").?);
    try std.testing.expect(parseProviderId("unknown") == null);
}

test "config get provider config" {
    const gpa = std.testing.allocator;
    var config = Config.init(gpa, ".rotary-test");
    defer {
        if (config.default_provider) |p| gpa.free(p);
        if (config.default_model) |m| gpa.free(m);
        var it = config.api_keys.iterator();
        while (it.next()) |entry| {
            gpa.free(entry.key_ptr.*);
            gpa.free(entry.value_ptr.*);
        }
        config.deinit();
    }

    config.default_provider = "openai";
    try config.api_keys.put("openai", "sk-test");

    const pc = config.getProviderConfig().?;
    try std.testing.expectEqual(provider.ProviderId.openai, pc.id);
    try std.testing.expectEqualStrings("sk-test", pc.api_key.?);
}
