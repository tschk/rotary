//! Computer-use tools via rs_peekaboo (crates.io dep — no FFI, no vendoring).

use crate::agent::{ToolDefinition, ToolRegistry, ToolResult};
use rs_peekaboo::automation::{parse_point, Target};
use rs_peekaboo::{Direction, ImageMode, Peekaboo, Point};
use serde_json::Value;
use std::sync::OnceLock;

static PEEKABOO: OnceLock<Peekaboo> = OnceLock::new();

fn peekaboo() -> &'static Peekaboo {
    PEEKABOO.get_or_init(Peekaboo::new)
}

pub fn register_tools(registry: &mut ToolRegistry) {
    registry.register(ToolDefinition {
        name: "cu_call".into(),
        description: "Call rs_peekaboo by method name with JSON args (extensible dispatch).".into(),
        parameters_json: r#"{"type":"object","properties":{"method":{"type":"string"},"args":{"type":"object"}},"required":["method"]}"#.into(),
        execute: |_ctx, args| Box::pin(exec_call(args)),
    });
    registry.register(ToolDefinition {
        name: "cu_see".into(),
        description: "Capture UI snapshot (optional app filter, mode, path, retina).".into(),
        parameters_json: r#"{"type":"object","properties":{"app":{"type":"string"},"mode":{"type":"string"},"path":{"type":"string"},"retina":{"type":"boolean"}}}"#.into(),
        execute: |_ctx, args| Box::pin(exec_see(args)),
    });
    registry.register(ToolDefinition {
        name: "cu_image".into(),
        description: "Screenshot screen/window/menu to path.".into(),
        parameters_json: r#"{"type":"object","properties":{"mode":{"type":"string"},"path":{"type":"string"},"retina":{"type":"boolean"}}}"#.into(),
        execute: |_ctx, args| Box::pin(exec_image(args)),
    });
    registry.register(ToolDefinition {
        name: "cu_click".into(),
        description: "Click at coords \"x,y\" or {x,y}. Optional button, count.".into(),
        parameters_json: r#"{"type":"object","properties":{"coords":{"type":"string"},"x":{"type":"integer"},"y":{"type":"integer"},"button":{"type":"string"},"count":{"type":"integer"}}}"#.into(),
        execute: |_ctx, args| Box::pin(exec_click(args)),
    });
    registry.register(ToolDefinition {
        name: "cu_type".into(),
        description: "Type text; optional clear, return, delay_ms, app.".into(),
        parameters_json: r#"{"type":"object","properties":{"text":{"type":"string"},"clear":{"type":"boolean"},"return":{"type":"boolean"},"delay_ms":{"type":"integer"},"app":{"type":"string"}},"required":["text"]}"#.into(),
        execute: |_ctx, args| Box::pin(exec_type(args)),
    });
    registry.register(ToolDefinition {
        name: "cu_hotkey".into(),
        description: "Hotkey chord e.g. \"cmd,l\" or \"cmd+l\".".into(),
        parameters_json:
            r#"{"type":"object","properties":{"keys":{"type":"string"}},"required":["keys"]}"#
                .into(),
        execute: |_ctx, args| Box::pin(exec_hotkey(args)),
    });
    registry.register(ToolDefinition {
        name: "cu_scroll".into(),
        description: "Scroll direction up|down|left|right, amount.".into(),
        parameters_json: r#"{"type":"object","properties":{"direction":{"type":"string"},"amount":{"type":"integer"}}}"#.into(),
        execute: |_ctx, args| Box::pin(exec_scroll(args)),
    });
    registry.register(ToolDefinition {
        name: "cu_window".into(),
        description: "Window list/focus/close/minimize. action + optional app, title.".into(),
        parameters_json: r#"{"type":"object","properties":{"action":{"type":"string"},"app":{"type":"string"},"title":{"type":"string"}}}"#.into(),
        execute: |_ctx, args| Box::pin(exec_window(args)),
    });
    registry.register(ToolDefinition {
        name: "cu_app".into(),
        description: "App list/launch/switch/quit. action + optional name.".into(),
        parameters_json: r#"{"type":"object","properties":{"action":{"type":"string"},"name":{"type":"string"}}}"#.into(),
        execute: |_ctx, args| Box::pin(exec_app(args)),
    });
    registry.register(ToolDefinition {
        name: "cu_list".into(),
        description: "List apps, windows, or screens.".into(),
        parameters_json: r#"{"type":"object","properties":{"what":{"type":"string"}}}"#.into(),
        execute: |_ctx, args| Box::pin(exec_list(args)),
    });
    registry.register(ToolDefinition {
        name: "cu_open".into(),
        description: "Open path or URL, optional app, no_focus.".into(),
        parameters_json: r#"{"type":"object","properties":{"target":{"type":"string"},"app":{"type":"string"},"no_focus":{"type":"boolean"}},"required":["target"]}"#.into(),
        execute: |_ctx, args| Box::pin(exec_open(args)),
    });
    registry.register(ToolDefinition {
        name: "cu_clipboard".into(),
        description: "Clipboard read/write. action read|write, optional text.".into(),
        parameters_json: r#"{"type":"object","properties":{"action":{"type":"string"},"text":{"type":"string"}},"required":["action"]}"#.into(),
        execute: |_ctx, args| Box::pin(exec_clipboard(args)),
    });
    tracing::info!("computer_use: registered rs_peekaboo tools (crates.io dep)");
}

fn parse_args(args: &str) -> Value {
    serde_json::from_str(args).unwrap_or(Value::Object(serde_json::Map::new()))
}

fn json_result(v: Result<Value, rs_peekaboo::PeekabooError>) -> ToolResult {
    match v {
        Ok(val) => ToolResult::ok("cu", serde_json::to_string(&val).unwrap_or_default()),
        Err(e) => ToolResult::err("cu", format!("{e}")),
    }
}

async fn exec_call(args: String) -> ToolResult {
    let v = parse_args(&args);
    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let a = v.get("args").cloned().unwrap_or(Value::Null);
    dispatch(method, &a)
}

async fn exec_see(args: String) -> ToolResult {
    let v = parse_args(&args);
    let app = v.get("app").and_then(|a| a.as_str());
    let mode = ImageMode::parse(v.get("mode").and_then(|m| m.as_str()).unwrap_or("screen"));
    let path = v
        .get("path")
        .and_then(|p| p.as_str())
        .map(std::path::PathBuf::from);
    let retina = v.get("retina").and_then(|r| r.as_bool()).unwrap_or(true);
    match peekaboo().see(app, mode, path, retina) {
        Ok(snap) => ToolResult::ok(
            "cu_see",
            serde_json::json!({"snapshot_id": snap.snapshot_id, "elements": snap.elements.len()})
                .to_string(),
        ),
        Err(e) => ToolResult::err("cu_see", format!("{e}")),
    }
}

async fn exec_image(args: String) -> ToolResult {
    let v = parse_args(&args);
    let mode = ImageMode::parse(v.get("mode").and_then(|m| m.as_str()).unwrap_or("screen"));
    let path = v
        .get("path")
        .and_then(|p| p.as_str())
        .map(std::path::PathBuf::from);
    let retina = v.get("retina").and_then(|r| r.as_bool()).unwrap_or(true);
    json_result(
        peekaboo()
            .image(mode, path, retina)
            .map(|i| serde_json::to_value(&i).unwrap_or(Value::Null)),
    )
}

async fn exec_click(args: String) -> ToolResult {
    let v = parse_args(&args);
    let target = if let Some(coords) = v.get("coords").and_then(|c| c.as_str()) {
        match parse_point(coords) {
            Ok(p) => Target::Point(p),
            Err(e) => return ToolResult::err("cu_click", format!("{e}")),
        }
    } else {
        let x = v.get("x").and_then(|x| x.as_i64()).unwrap_or(0);
        let y = v.get("y").and_then(|y| y.as_i64()).unwrap_or(0);
        Target::Point(Point { x, y })
    };
    let button = v.get("button").and_then(|b| b.as_str()).unwrap_or("left");
    let count = v.get("count").and_then(|c| c.as_u64()).unwrap_or(1) as u32;
    json_result(peekaboo().click(target, button, count))
}

async fn exec_type(args: String) -> ToolResult {
    let v = parse_args(&args);
    let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let clear = v.get("clear").and_then(|c| c.as_bool()).unwrap_or(false);
    let press_return = v.get("return").and_then(|r| r.as_bool()).unwrap_or(false);
    let delay_ms = v.get("delay_ms").and_then(|d| d.as_u64());
    let app = v.get("app").and_then(|a| a.as_str());
    json_result(peekaboo().type_text(text, clear, press_return, delay_ms, app))
}

async fn exec_hotkey(args: String) -> ToolResult {
    let v = parse_args(&args);
    let keys = v.get("keys").and_then(|k| k.as_str()).unwrap_or("");
    let parts: Vec<&str> = keys
        .split([',', '+'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    json_result(peekaboo().hotkey(&parts))
}

async fn exec_scroll(args: String) -> ToolResult {
    let v = parse_args(&args);
    let dir = Direction::parse(
        v.get("direction")
            .and_then(|d| d.as_str())
            .unwrap_or("down"),
    );
    let amount = v.get("amount").and_then(|a| a.as_u64()).unwrap_or(3) as u32;
    json_result(peekaboo().scroll(dir, amount.max(1)))
}

async fn exec_window(args: String) -> ToolResult {
    let v = parse_args(&args);
    let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("list");
    let app = v.get("app").and_then(|a| a.as_str());
    let title = v.get("title").and_then(|t| t.as_str());
    json_result(peekaboo().window(action, app, title, None))
}

async fn exec_app(args: String) -> ToolResult {
    let v = parse_args(&args);
    let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("list");
    let name = v.get("name").and_then(|n| n.as_str());
    json_result(peekaboo().app(action, name))
}

async fn exec_list(args: String) -> ToolResult {
    let v = parse_args(&args);
    let what = v.get("what").and_then(|w| w.as_str()).unwrap_or("apps");
    match what {
        "windows" => json_result(peekaboo().list_windows()),
        "screens" => json_result(peekaboo().list_screens()),
        _ => json_result(peekaboo().list_apps()),
    }
}

async fn exec_open(args: String) -> ToolResult {
    let v = parse_args(&args);
    let target = match v.get("target").and_then(|t| t.as_str()) {
        Some(t) => t,
        None => return ToolResult::err("cu_open", "target required"),
    };
    let app = v.get("app").and_then(|a| a.as_str());
    let no_focus = v.get("no_focus").and_then(|n| n.as_bool()).unwrap_or(false);
    json_result(peekaboo().open(target, app, no_focus))
}

async fn exec_clipboard(args: String) -> ToolResult {
    let v = parse_args(&args);
    let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("read");
    match action {
        "read" => match peekaboo().clipboard_read() {
            Ok(text) => ToolResult::ok("cu_clipboard", text),
            Err(e) => ToolResult::err("cu_clipboard", format!("{e}")),
        },
        "write" => {
            let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
            json_result(peekaboo().clipboard_write(text))
        }
        _ => ToolResult::err("cu_clipboard", "action must be read or write"),
    }
}

fn dispatch(method: &str, args: &Value) -> ToolResult {
    let p = peekaboo();
    let result = match method {
        "see" => {
            let app = args.get("app").and_then(|a| a.as_str());
            let mode = ImageMode::parse(
                args.get("mode")
                    .and_then(|m| m.as_str())
                    .unwrap_or("screen"),
            );
            let path = args
                .get("path")
                .and_then(|p| p.as_str())
                .map(std::path::PathBuf::from);
            let retina = args.get("retina").and_then(|r| r.as_bool()).unwrap_or(true);
            p.see(app, mode, path, retina).map(
                |s| serde_json::json!({"snapshot_id": s.snapshot_id, "elements": s.elements.len()}),
            )
        }
        "image" => {
            let mode = ImageMode::parse(
                args.get("mode")
                    .and_then(|m| m.as_str())
                    .unwrap_or("screen"),
            );
            let path = args
                .get("path")
                .and_then(|p| p.as_str())
                .map(std::path::PathBuf::from);
            let retina = args.get("retina").and_then(|r| r.as_bool()).unwrap_or(true);
            p.image(mode, path, retina)
                .map(|i| serde_json::to_value(&i).unwrap_or(Value::Null))
        }
        "click" => {
            let target = if let Some(coords) = args.get("coords").and_then(|c| c.as_str()) {
                match parse_point(coords) {
                    Ok(pt) => Target::Point(pt),
                    Err(e) => return ToolResult::err("cu_call", format!("{e}")),
                }
            } else {
                let x = args.get("x").and_then(|x| x.as_i64()).unwrap_or(0);
                let y = args.get("y").and_then(|y| y.as_i64()).unwrap_or(0);
                Target::Point(Point { x, y })
            };
            let button = args
                .get("button")
                .and_then(|b| b.as_str())
                .unwrap_or("left");
            let count = args.get("count").and_then(|c| c.as_u64()).unwrap_or(1) as u32;
            p.click(target, button, count)
        }
        "type" => {
            let text = args.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let clear = args.get("clear").and_then(|c| c.as_bool()).unwrap_or(false);
            let press_return = args
                .get("return")
                .and_then(|r| r.as_bool())
                .unwrap_or(false);
            let delay_ms = args.get("delay_ms").and_then(|d| d.as_u64());
            let app = args.get("app").and_then(|a| a.as_str());
            p.type_text(text, clear, press_return, delay_ms, app)
        }
        "hotkey" => {
            let keys = args.get("keys").and_then(|k| k.as_str()).unwrap_or("");
            let parts: Vec<&str> = keys
                .split([',', '+'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            p.hotkey(&parts)
        }
        "scroll" => {
            let dir = Direction::parse(
                args.get("direction")
                    .and_then(|d| d.as_str())
                    .unwrap_or("down"),
            );
            let amount = args.get("amount").and_then(|a| a.as_u64()).unwrap_or(3) as u32;
            p.scroll(dir, amount.max(1))
        }
        "window" => {
            let action = args
                .get("action")
                .and_then(|a| a.as_str())
                .unwrap_or("list");
            let app = args.get("app").and_then(|a| a.as_str());
            let title = args.get("title").and_then(|t| t.as_str());
            p.window(action, app, title, None)
        }
        "app" => {
            let action = args
                .get("action")
                .and_then(|a| a.as_str())
                .unwrap_or("list");
            let name = args.get("name").and_then(|n| n.as_str());
            p.app(action, name)
        }
        "open" => {
            let target = match args.get("target").and_then(|t| t.as_str()) {
                Some(t) => t,
                None => return ToolResult::err("cu_call", "target required for open"),
            };
            let app = args.get("app").and_then(|a| a.as_str());
            let no_focus = args
                .get("no_focus")
                .and_then(|n| n.as_bool())
                .unwrap_or(false);
            p.open(target, app, no_focus)
        }
        "list_apps" => p.list_apps(),
        "list_windows" => p.list_windows(),
        "list_screens" => p.list_screens(),
        "permissions" => Ok(p.permissions()),
        "clipboard_read" => p.clipboard_read().map(Value::String),
        "clipboard_write" => {
            let text = args.get("text").and_then(|t| t.as_str()).unwrap_or("");
            p.clipboard_write(text)
        }
        other => return ToolResult::err("cu_call", format!("unknown method: {other}")),
    };
    json_result(result)
}
