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
| `cu_click` | Click at screen coordinates |
| `cu_type` | Type text into the focused element |
| `cu_hotkey` | Press a keyboard hotkey / key combination |
| `cu_scroll` | Scroll at coordinates or in the focused element |
| `cu_window` | Focus, move, resize, or close a window |
| `cu_app` | Launch or switch to an application |
| `cu_list` | List open windows or running applications |
| `cu_open` | Open a file or URL in the default handler |
| `cu_clipboard` | Read from or write to the system clipboard |
| `cu_doctor` | Diagnose computer-use environment and permissions |
