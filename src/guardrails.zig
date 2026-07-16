const std = @import("std");

/// Turn guardrails inspired by Zero's runaway-model protection.
pub const Config = struct {
    max_empty_turns: u32 = 3,
    max_consecutive_tool_failures: u32 = 6,
    max_tool_iterations: u32 = 20,
    stale_tool_call_threshold: u32 = 12,
};

pub const State = struct {
    empty_turns: u32 = 0,
    consecutive_tool_failures: u32 = 0,
    tool_calls_since_progress: u32 = 0,

    pub fn noteEmptyTurn(self: *State) void {
        self.empty_turns += 1;
    }

    pub fn noteContent(self: *State) void {
        self.empty_turns = 0;
        self.tool_calls_since_progress = 0;
    }

    pub fn noteToolFailure(self: *State) void {
        self.consecutive_tool_failures += 1;
        self.tool_calls_since_progress += 1;
    }

    pub fn noteToolSuccess(self: *State) void {
        self.consecutive_tool_failures = 0;
        self.tool_calls_since_progress += 1;
    }

    pub fn shouldStop(self: State, cfg: Config) bool {
        if (self.empty_turns >= cfg.max_empty_turns) return true;
        if (self.consecutive_tool_failures >= cfg.max_consecutive_tool_failures) return true;
        if (self.tool_calls_since_progress >= cfg.stale_tool_call_threshold) return true;
        return false;
    }

    pub fn reason(self: State, cfg: Config) ?[]const u8 {
        if (self.empty_turns >= cfg.max_empty_turns) return "too many empty turns";
        if (self.consecutive_tool_failures >= cfg.max_consecutive_tool_failures) return "repeated tool failures";
        if (self.tool_calls_since_progress >= cfg.stale_tool_call_threshold) return "stale tool loop";
        return null;
    }
};

test "guardrails trip on empty turns" {
    var state = State{};
    const cfg = Config{ .max_empty_turns = 2 };
    state.noteEmptyTurn();
    try std.testing.expect(!state.shouldStop(cfg));
    state.noteEmptyTurn();
    try std.testing.expect(state.shouldStop(cfg));
}
