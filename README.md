# rotary (rx4) — the agent harness engine

[![crates.io](https://img.shields.io/crates/v/rx4.svg)](https://crates.io/crates/rx4)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL--2.0-blue.svg)](LICENSE)
[![MSRV: 1.88](https://img.shields.io/badge/MSRV-1.88-blue.svg)](https://blog.rust-lang.org/2025/06/26/Rust-1.88.0.html)

Pure agent harness engine. Models write; rotary gives them tools, memory, loops, permissions, sessions, and control planes.

## Install

Library hosts should disable crate defaults and name only the features they need:

```bash
cargo add rx4 --no-default-features --features builtin-tools,providers
```

The `rx4` binary is gated on the `cli` feature. A bare `cargo install rx4` does **not** produce the binary:

```bash
cargo install rx4 --features cli,ipc,providers,builtin-tools,mcp
```

See [docs/INSTALL.md](docs/INSTALL.md) for Homebrew notes and the full feature list.

## Quick start

```rust
use rx4::{Agent, Scope, ToolRegistry, register_builtin_tools};

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

## Docs

### Hashline, prewalk, AVO

The engine owns the tagged edit protocol (`rx4::hashline`), the one-way
investigate→smol switch (`rx4::prewalk`, `RX4_SMOL_MODEL`), and AVO helpers
(`rx4::avo`: `P_t`, two-part `f`, commit-if-better, stall). Hosts enable them;
they should not fork the protocol. See `docs/HARNESS.md`.

### Opt-in autoresearch

- [Documentation index](docs/README.md)
- [Hosting guide](docs/HOSTS.md)
- **[telekinesis](https://github.com/tschk/telekinesis)** — CLI/TUI product host
- **[apollo](https://github.com/tschk/apollo)** — AI agent host

## License

MPL-2.0
