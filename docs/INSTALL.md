# Install

Rotary is the product name for the MPL-2.0 agent harness crate and CLI. The Rust crate and executable are both named `rx4`; use `rx4`, not `rotary`, in terminal commands.

## Library (hosts)

Disable crate defaults and name only the features the host needs:

```bash
cargo add rx4 --no-default-features --features builtin-tools,providers
```

`Policy::default()` and `Agent::new` use `workspace_write`. Hosts that need a binary stay on the `cli` feature; slim embeds leave it off.

See [FEATURES.md](FEATURES.md) for the full flag table.

## CLI binary

The `rx4` binary is gated on the `cli` feature (`required-features = ["cli"]`). A bare `cargo install rx4` does **not** produce the binary. Include `cli`:

```bash
cargo install rx4 --features cli,ipc,providers,builtin-tools,mcp
```

```bash
rx4 chat
rx4 exec "fix the failing test"
rx4 serve /tmp/rx4.sock
rx4 doctor
rx4 models
rx4 tools
```

`rx4 exec --stream-json` emits NDJSON agent events.

## Homebrew

Homebrew `undivisible/homebrew-tap` `Formula/rx4.rb` currently runs

```bash
cargo install --locked --path . --features providers,ipc,builtin-tools,mcp
```

and will ship no binary until that formula adds `cli`.
