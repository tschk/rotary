# rotary

**The agent harness engine.** Models write; rotary gives them tools, memory, loops, permissions, sessions, and control planes. Hosts (CLIs, TUIs, IDEs) embed rotary.

[![crates.io](https://img.shields.io/crates/v/rotary.svg)](https://crates.io/crates/rotary)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL--2.0-blue.svg)](LICENSE)

## Install

```toml
[dependencies]
rotary = { version = "0.3", features = ["ipc", "computer-use"] }
```

## Quick start

```rust
use rotary::{Agent, Scope, ToolRegistry, register_builtin_tools};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut agent = Agent::new();
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);
    agent.set_tools(tools);
    agent.set_scope(Scope::Coding);
    agent.prompt("fix the failing test").await?;
    Ok(())
}
```

## IPC server

```bash
rotary serve /tmp/rotary.sock
```

JSON-RPC methods: `ping`, `state`, `prompt`, `set_model`, `tools`, `plugins`, `messages`, `session_list`, `session_clear`.

## Scopes

| Scope | Tools | Policy |
|---|---|---|
| `coding` | FS + shell + find | workspace_write |
| `research` | read-only | read_only |
| `plan` | read-only | read_only |
| `ask` | none | deny_all |
| `computer_use` | rs_peekaboo `cu_*` | full_access |

## Computer-use

Powered by [rs_peekaboo](https://crates.io/crates/rs_peekaboo) — native Rust, no FFI:

```toml
rotary = { version = "0.3", features = ["computer-use"] }
```

Tools: `cu_call`, `cu_see`, `cu_image`, `cu_click`, `cu_type`, `cu_hotkey`, `cu_scroll`, `cu_window`, `cu_app`, `cu_list`, `cu_open`, `cu_clipboard`.

## License

MPL-2.0
