# Hosting the rotary harness engine

## Architecture split

```
┌─────────────────────────────────────┐
│ telekinesis (CLI + crepuscularity-tui) │
│ product UX, slash palette, streaming UI │
└──────────────────┬──────────────────┘
                   │ JSON-RPC IPC (typed events)
┌──────────────────▼──────────────────┐
│ rotary — agent harness engine         │
│ loop · tools · providers · sessions   │
│ permissions · hooks · scopes · FFI    │
└─────────────────────────────────────┘
```

## telekinesis

Primary host. One CLI, one TUI.

```bash
telekinesis serve          # rotary-backed IPC daemon
telekinesis                # default: launch TUI (after serve)
telekinesis exec "prompt"  # non-interactive (Codex/OpenCode-style)
telekinesis agent|provider|session|…
```

Wire:

- Submodule: `vendor/rotary`
- Build: `rotary` module import in `build.zig`
- `src/root.zig` re-exports rotary APIs
- `ui/tui` (crepuscularity-tui) talks only over IPC — never imports harness internals

## Embedding as a library

```zig
const rotary = @import("rotary");
var agent = rotary.Agent.init(gpa, io);
try agent.setScope(.coding);
```

Or attach as IPC-only: start rotary’s server surface through telekinesis `serve`.

## Computer-use FFI (equilibrium)

Do **not** shell out to `rs-peekaboo`. Embed the crate:

1. `vendor/rs_peekaboo` — library
2. `native/peekaboo_ffi` — `extern "C"` staticlib
3. Generate Zig consumers with equilibrium:

```bash
cargo install --git https://github.com/tschk/equilibrium --features cli
eq generate include/peekaboo.h --consumer zig -o src/generated/peekaboo.zig
zig build -Dpeekaboo=true
```

Regenerate bindings whenever the C header changes. Prefer the `cu_call` tool (`method` + JSON args) so the ABI stays small.
