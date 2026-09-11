# rotary (rx4) — the agent harness engine

[![crates.io](https://img.shields.io/crates/v/rx4.svg)](https://crates.io/crates/rx4)
[![License: MPL-2.0](https://img.shields.io/badge/License-MPL--2.0-blue.svg)](LICENSE)
[![MSRV: 1.88](https://img.shields.io/badge/MSRV-1.88-blue.svg)](https://blog.rust-lang.org/2025/06/26/Rust-1.88.0.html)

Pure agent harness engine. Models write; rotary gives them tools, memory, loops, permissions, sessions, and control planes. Hosts embed it as a library or drive it over IPC; policy and UI stay in the host.

## Quick start

### Library

Slim hosts disable crate defaults and name only the features they need:

```bash
cargo add rx4 --no-default-features --features builtin-tools,providers
```

```rust
use rx4::{register_builtin_tools, Agent, Scope, ToolRegistry};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut agent = Agent::new();

    let tools = ToolRegistry::new();
    register_builtin_tools(&tools);
    agent.set_tools(tools);
    agent.set_scope(Scope::Coding);
    agent.set_policy(rx4::Policy::workspace_write());

    // A prompt needs a provider; see `examples/provider_agent.rs`.
    // agent.prompt("fix the failing test").await?;
    Ok(())
}
```

Run the bundled examples:

```bash
cargo run --example minimal_agent
cargo run --example custom_tool
cargo run --example sessions
OPENAI_API_KEY=sk-... cargo run --example provider_agent --features providers
```

See [examples/README.md](examples/README.md) for what each one covers.

### Binary

The `rx4` binary is gated on the `cli` feature, so a bare `cargo install rx4`
does **not** produce it:

```bash
cargo install rx4 --features cli,ipc,providers,builtin-tools,mcp
```

```bash
rx4 chat
rx4 exec "fix the failing test"
rx4 serve /tmp/rx4.sock
rx4 doctor
```

See [docs/INSTALL.md](docs/INSTALL.md) for Homebrew notes and
[docs/FEATURES.md](docs/FEATURES.md) for the full feature flag table.

## What it exposes

A scope is a work mode, not an agent name.

- **Agent loop** with streaming events, parallel tool batches, and guardrails.
- **Builtin tool loadout** — file (`read`/`write`/`edit`), shell (`bash`/`exec`), search (`grep`/`find`/`ls`), plus `web_fetch`, `todo`, `spawn_agent`, plan-mode, and LSP helpers.
- **Scopes** — `coding`, `research`, `plan`, `ask`, `computer_use`.
- **Permissions** — `Policy` + pluggable `Authorizer`; `workspace_write` default.
- **Providers** — OpenAI, Anthropic, Ollama, and any OpenAI-compatible endpoint.
- **Sessions** — fork/merge tree with JSONL and optional SQLite persistence.
- **Skills, graph memory, dream scheduler, computer-use, MCP, LSP** — opt-in capabilities.

rotary exposes capabilities, not policy: scheduling, enabled flags, and lifecycle decisions belong to the host.

### Hashline, prewalk, AVO

The engine owns the tagged edit protocol (`rx4::hashline`), the one-way investigate→smol switch (`rx4::prewalk`, `RX4_SMOL_MODEL`), and AVO helpers (`rx4::avo`: `P_t`, two-part `f`, commit-if-better, stall). Hosts enable them; they should not fork the protocol. See [docs/HARNESS.md](docs/HARNESS.md).

### Opt-in autoresearch

`AutoresearchSession` persists `.auto/` metadata; `AutoresearchController` adds detached Git worktrees, checkpoint rollback, median aggregation, guards, budgets, typed events, and explicit final-patch acceptance. It is host-driven and never mutates the real checkout automatically. See [docs/AUTORESEARCH.md](docs/AUTORESEARCH.md).

## Docs

- [Documentation index](docs/README.md)
- [Install](docs/INSTALL.md) · [Features](docs/FEATURES.md) · [Providers](docs/PROVIDERS.md) · [Events](docs/EVENTS.md)
- [Hosting guide](docs/HOSTS.md) · [Architecture](docs/ARCHITECTURE.md) · [Computer-use](docs/COMPUTER-USE.md)
- [telekinesis](https://github.com/tschk/telekinesis) — CLI/TUI product host
- [apollo](https://github.com/tschk/apollo) — AI agent host

## License

MPL-2.0
