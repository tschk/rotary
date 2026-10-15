# Computer-use

Powered by [Praefectus](https://crates.io/crates/praefectus) — native Rust, no FFI. Do **not** shell out to Praefectus; embed it through rx4's `computer-use` feature.

```toml
rx4 = { version = "0.6", features = ["computer-use"] }
```

```rust
rx4::computer_use::register_tools(&mut tools);
```

This registers 13 `cu_*` tools. ComputerUse defaults to `workspace_write`; hosts opt into `full_access`.

| Tool | Description |
|---|---|
| `cu_call` | Invoke a named application method or open a target |
| `cu_see` | Capture a screenshot / visual snapshot of the screen |
| `cu_image` | Encode or transform an image for model input |
| `cu_click` | Invoke an observed semantic element (background-safe); coordinate clicks are a last resort |
| `cu_type` | Type text into the focused element of the frontmost app |
| `cu_hotkey` | Press a keyboard hotkey / key combination |
| `cu_scroll` | Scroll the focused element of the frontmost app |
| `cu_window` | List windows, or focus, close, or minimize one (focus steals the foreground) |
| `cu_app` | List applications, or launch, switch to, or quit one (switch steals the foreground) |
| `cu_list` | List applications, windows, screens, or background surfaces |
| `cu_open` | Open a file or URL in the default handler |
| `cu_clipboard` | Read from or write to the system clipboard |
| `cu_doctor` | Diagnose computer-use environment and permissions |

## Background-first interaction

Background interaction is the **enforced default**. Semantic actions
(`invoke`, `set_value`) are delivered straight to the target element's
process and never move the cursor; on macOS the same is true of input aimed
at a fenced element. The bridge requests `background_only` for those routes,
and Praefectus samples the foreground window, focus, and pointer around every
background action — if the desktop changes, the result is `outcome_unknown`,
never a silent success.

Foreground-bound routes — coordinate clicks, window focus, app launch and
switch — are refused by the bridge unless the host sets
`PRAEFECTUS_ALLOW_GLOBAL_INPUT=1` (the same opt-in Praefectus requires before
it will deliver global pointer input). With the flag unset, agents cannot
move the cursor or steal focus through the `cu_*` tools at all; every
foreground seizure is a deliberate host decision, never a model fallback.

Prefer this loop: `cu_list what=surfaces` to enumerate background surfaces,
`cu_see surface=<id>` to observe one without activating it, then
`cu_click` on an observed element tag or set values on observed fields.
