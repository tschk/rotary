# rotary examples

Runnable examples for embedding the `rx4` agent harness. Each one is small and
builds against the crate as published on crates.io.

| Example | Feature | Run |
|---|---|---|
| [minimal_agent](minimal_agent.rs) | `builtin-tools` (default) | `cargo run --example minimal_agent` |
| [custom_tool](custom_tool.rs) | `builtin-tools` (default) | `cargo run --example custom_tool` |
| [sessions](sessions.rs) | default | `cargo run --example sessions` |
| [provider_agent](provider_agent.rs) | `providers` | `OPENAI_API_KEY=sk-... cargo run --example provider_agent --features providers` |

## What each example shows

### `minimal_agent`
The smallest useful embed: `Agent::new`, `register_builtin_tools`, `set_scope`,
`set_policy`, and loadout introspection via `ToolRegistry::names` /
`definitions_fingerprint`.

### `custom_tool`
Register a host tool with `ToolDefinition::new_fn`, mark its `ToolEffect`, and
invoke it through `ToolRegistry::execute` with a `ToolContext`.

### `sessions`
The session tree: append entries by `Role`, `fork` at an entry id, `merge` a
branch back, then `save_jsonl` / `load_jsonl` for persistence.

### `provider_agent`
Wire an `OpenAIProvider`, `subscribe` to `Event`s for streaming deltas, tool
calls, and turn completion, then drive one `prompt`. Requires the `providers`
feature and an API key; exits early when none is set.

## Notes

- Examples use `--no-default-features` only when trimming dependencies. The
  default feature set includes `builtin-tools`.
- A library host that wants the `rx4` binary instead of a slim embed should
  enable `cli`; see the install section of the [main README](../README.md).
