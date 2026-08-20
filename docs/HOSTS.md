# Hosting the rotary harness engine

## Architecture split

```mermaid
graph TD
  subgraph Host["Hosts: telekinesis and Omi Desktop"]
    UX["product UX · slash palette · streaming UI<br/>pi protocol compat (JSONL v3, RPC, extensions)"]
    Runtime["host runtime"]
  end
  Host -->|typed calls and events| Runtime
  Runtime -->|in-process first; transport later| Engine
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
```

Wire:

- `rx4` is a published Cargo dependency, not a submodule.
- `ui/tui/src/main.rs` currently imports rx4 directly and drives the agent loop
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

The existing `rx4 serve` surface remains a compatibility adapter during the
boundary migration. New host surfaces should use telekinesis `HostRuntime`.

## Computer-use

Do **not** shell out to Praefectus. Embed it through rx4's `computer-use`
feature. See [COMPUTER-USE.md](COMPUTER-USE.md) for the tool table.

```rust
rx4::computer_use::register_tools(&mut tools);
```

This registers the 13 `cu_*` tools with no FFI through the native Rust
Praefectus crate from crates.io.

## Compatibility IPC adapter

```bash
rx4 serve /tmp/rx4.sock
```

JSON-RPC methods: `ping`, `state`, `prompt`, `set_model`, `tools`,
`plugins`, `messages`, `session_list`, `session_clear`.

Socket mode is `0o600`. This adapter remains for compatibility while
telekinesis owns the product host boundary. Optional auth: set
`RX4_IPC_TOKEN` and pass
`"token"` in each JSON-RPC `params` object (fail-open when unset — local
socket only).

> `rx4 serve` starts the Unix socket JSON-RPC server. Hosts connect to
> the socket and drive the agent loop remotely — the host never owns agent
> logic.

## apollo

[apollo](https://github.com/tschk/apollo) is an AI agent host along the likes of Hermes. omi PR + beta versions also embed rotary.
