# Hosting the rotary harness engine

## Architecture split

```mermaid
graph TD
  subgraph Host["Hosts: telekinesis and Omi Desktop"]
    UX["product UX · slash palette · streaming UI<br/>pi protocol compat (JSONL v3, RPC, extensions)"]
  end
  Host -->|tokio channels (in-process)<br/>or JSON-RPC IPC| Engine
  subgraph Engine["rotary — agent harness engine"]
    Loop["loop · tools · providers · sessions"]
    Ctrl["permissions · hooks · scopes · guardrails"]
    Skills["skills · curator · background review · dream"]
  end
```

## telekinesis

Primary host. One CLI, one TUI.

```bash
tk                          # launch the telekinesis TUI
rx4 serve /tmp/rx4.sock     # optional Rotary IPC daemon
```

Wire:

- `rx4` is a **Cargo dependency** (`rx4 = "0.3"` in `ui/tui/Cargo.toml`),
  not a submodule.
- `ui/tui/src/main.rs` imports rx4 directly and drives the agent loop
  in-process via tokio channels.
- builtin tools + computer-use tools are registered at startup.
- pi protocol compat (JSONL v3 sessions, RPC, extensions, QuickJS) is owned
  by telekinesis, not rotary.

## Embedding as a library

```rust
use rx4::{Agent, Scope, ToolRegistry, register_builtin_tools};

let mut agent = Agent::new();
let mut tools = ToolRegistry::new();
register_builtin_tools(&mut tools);
agent.set_tools(tools);
agent.set_scope(Scope::Coding);
```

Or attach as IPC-only: start Rotary's server surface through `rx4 serve`.

## Computer-use

Do **not** shell out to `rs-peekaboo`. Embed the crate via the
`computer-use` feature:

```toml
rx4 = { version = "0.3", features = ["computer-use"] }
```

```rust
rx4::computer_use::register_tools(&mut tools);
```

This registers the 13 `cu_*` tools with no FFI — rs_peekaboo is a native
Rust crate from crates.io.
