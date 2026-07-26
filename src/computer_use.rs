//! Computer-use tools via rs_peekaboo (crates.io dep — no FFI, no vendoring).

use crate::agent::{ToolDefinition, ToolEffect, ToolExecutor, ToolRegistry, ToolResult};
use crate::computer_use_bridge::ComputerUseBridge;
use praefectus::{
    Action, ApplicationOperation, CancellationToken, Direction, MouseButton, SafetyClass,
    TargetRef, VerificationPolicy, WindowOperation,
};
use serde_json::Value;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static BRIDGE: OnceLock<Result<Arc<ComputerUseBridge>, String>> = OnceLock::new();

fn bridge() -> Result<Arc<ComputerUseBridge>, String> {
    BRIDGE
        .get_or_init(|| ComputerUseBridge::new().map(Arc::new))
        .clone()
}

pub fn register_tools(registry: &mut ToolRegistry) {
    register(
        registry,
        "cu_call",
        "Call a Praefectus computer-use method with JSON args.",
        r#"{"type":"object","properties":{"method":{"type":"string"},"args":{"type":"object"}},"required":["method"]}"#,
        ToolEffect::Process,
    );
    register(
        registry,
        "cu_see",
        "Observe the active UI as a bounded semantic snapshot.",
        r#"{"type":"object","properties":{"path":{"type":"string"}}}"#,
        ToolEffect::Write,
    );
    register(
        registry,
        "cu_image",
        "Capture the active screen to a workspace path.",
        r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
        ToolEffect::Write,
    );
    register(
        registry,
        "cu_click",
        "Invoke a semantic element tag or click freshly observed coordinates.",
        r#"{"type":"object","properties":{"coords":{"type":"string"},"x":{"type":"integer"},"y":{"type":"integer"},"index":{"type":"integer"},"snapshot":{"type":"string"},"on":{"type":"string"},"button":{"type":"string"},"count":{"type":"integer"}}}"#,
        ToolEffect::Process,
    );
    register(
        registry,
        "cu_type",
        "Type text into the exactly observed focused element.",
        r#"{"type":"object","properties":{"text":{"type":"string"},"clear":{"type":"boolean"},"return":{"type":"boolean"},"delay_ms":{"type":"integer"}},"required":["text"]}"#,
        ToolEffect::Process,
    );
    register(
        registry,
        "cu_hotkey",
        "Send a hotkey to the exactly observed focused element.",
        r#"{"type":"object","properties":{"keys":{"type":"string"}},"required":["keys"]}"#,
        ToolEffect::Process,
    );
    register(
        registry,
        "cu_scroll",
        "Scroll the exactly observed focused element.",
        r#"{"type":"object","properties":{"direction":{"type":"string"},"amount":{"type":"integer"}}}"#,
        ToolEffect::Process,
    );
    register(
        registry,
        "cu_window",
        "List, focus, close, or minimize windows.",
        r#"{"type":"object","properties":{"action":{"type":"string"},"app":{"type":"string"},"title":{"type":"string"}}}"#,
        ToolEffect::Process,
    );
    register(
        registry,
        "cu_app",
        "List, launch, switch, or quit applications.",
        r#"{"type":"object","properties":{"action":{"type":"string"},"name":{"type":"string"}}}"#,
        ToolEffect::Process,
    );
    register(
        registry,
        "cu_list",
        "List applications, windows, or screens.",
        r#"{"type":"object","properties":{"what":{"type":"string"}}}"#,
        ToolEffect::Read,
    );
    register(
        registry,
        "cu_open",
        "Open a path or URL with an optional application.",
        r#"{"type":"object","properties":{"target":{"type":"string"},"app":{"type":"string"},"no_focus":{"type":"boolean"}},"required":["target"]}"#,
        ToolEffect::Process,
    );
    register(
        registry,
        "cu_clipboard",
        "Read or write the clipboard.",
        r#"{"type":"object","properties":{"action":{"type":"string"},"text":{"type":"string"}},"required":["action"]}"#,
        ToolEffect::Process,
    );
    register(
        registry,
        "cu_doctor",
        "Report Praefectus computer-use readiness and capabilities.",
        r#"{"type":"object","properties":{}}"#,
        ToolEffect::Read,
    );
    tracing::info!("computer_use: registered Praefectus tools");
}

fn register(
    registry: &mut ToolRegistry,
    name: &'static str,
    description: &'static str,
    parameters_json: &'static str,
    effect: ToolEffect,
) {
    registry.register(ToolDefinition {
        name: name.into(),
        description: description.into(),
        parameters_json: parameters_json.into(),
        execute: ToolExecutor::Boxed(Box::new(move |ctx, args| {
            Box::pin(async move {
                let bridge = match bridge() {
                    Ok(bridge) => bridge,
                    Err(error) => return ToolResult::err(name, error),
                };
                let args = serde_json::from_str(&args)
                    .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
                match execute_named(&bridge, &ctx, name, &args) {
                    Ok(value) => ToolResult::ok(
                        name,
                        serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string()),
                    ),
                    Err(error) => ToolResult::err(name, error),
                }
            })
        })),
        effect,
    });
}

fn execute_named(
    bridge: &ComputerUseBridge,
    ctx: &crate::agent::ToolContext,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    let deadline = deadline();
    let cancellation = CancellationToken::default();
    #[cfg(feature = "ipc")]
    let _cancellation_registration =
        ctx.cancellation
            .register(cancellation_token::CancelCallback::FnOnce(Box::new({
                let cancellation = cancellation.clone();
                move || cancellation.cancel()
            })));
    match name {
        "cu_call" => {
            let method = required_string(args, "method")?;
            let nested = args.get("args").unwrap_or(&Value::Null);
            let name = if method.starts_with("cu_") {
                method.to_string()
            } else {
                format!("cu_{method}")
            };
            if name == "cu_call" {
                return Err("recursive cu_call is unavailable".to_string());
            }
            execute_named(bridge, ctx, &name, nested)
        }
        "cu_see" => {
            if let Some(path) = args.get("path").and_then(Value::as_str) {
                screenshot(bridge, ctx, path, &cancellation)?;
            }
            let observation = bridge
                .observer()
                .observe_semantic(&cancellation, deadline)
                .map_err(|error| error.to_string())?;
            bridge.set_observation(observation.clone());
            serde_json::to_value(observation).map_err(|error| error.to_string())
        }
        "cu_image" => screenshot(bridge, ctx, required_string(args, "path")?, &cancellation),
        "cu_click" => click(bridge, args, &cancellation),
        "cu_type" => {
            let target = bridge
                .observer()
                .observe_focused(&cancellation, deadline)
                .map_err(|error| error.to_string())?;
            bridge.execute(
                Action::TypeText {
                    text: required_string(args, "text")?.to_string(),
                    clear: args.get("clear").and_then(Value::as_bool).unwrap_or(false),
                    press_return: args.get("return").and_then(Value::as_bool).unwrap_or(false),
                    delay_ms: args.get("delay_ms").and_then(Value::as_u64),
                },
                TargetRef::Element { target },
                VerificationPolicy::None,
                SafetyClass::Reversible,
                &cancellation,
            )
        }
        "cu_hotkey" => {
            let target = bridge
                .observer()
                .observe_focused(&cancellation, deadline)
                .map_err(|error| error.to_string())?;
            let keys = required_string(args, "keys")?
                .split([',', '+'])
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            if keys.is_empty() {
                return Err("keys required".to_string());
            }
            bridge.execute(
                Action::Hotkey { keys },
                TargetRef::Element { target },
                VerificationPolicy::None,
                SafetyClass::Reversible,
                &cancellation,
            )
        }
        "cu_scroll" => {
            let target = bridge
                .observer()
                .observe_focused(&cancellation, deadline)
                .map_err(|error| error.to_string())?;
            bridge.execute(
                Action::Scroll {
                    direction: direction(args.get("direction").and_then(Value::as_str)),
                    amount: u32::try_from(args.get("amount").and_then(Value::as_u64).unwrap_or(3))
                        .unwrap_or(u32::MAX)
                        .max(1),
                },
                TargetRef::Element { target },
                VerificationPolicy::None,
                SafetyClass::Reversible,
                &cancellation,
            )
        }
        "cu_window" => window(bridge, args, &cancellation, deadline),
        "cu_app" => application(bridge, args, &cancellation, deadline),
        "cu_list" => list(bridge, args, &cancellation, deadline),
        "cu_open" => bridge.execute(
            Action::Open {
                target: required_string(args, "target")?.to_string(),
                app: args.get("app").and_then(Value::as_str).map(str::to_string),
                no_focus: args
                    .get("no_focus")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
            TargetRef::None,
            VerificationPolicy::None,
            SafetyClass::External,
            &cancellation,
        ),
        "cu_clipboard" => clipboard(bridge, args, &cancellation, deadline),
        "cu_doctor" => bridge
            .observer()
            .doctor(&cancellation, deadline)
            .map_err(|error| error.to_string()),
        _ => Err(format!("unknown computer-use method `{name}`")),
    }
}

fn screenshot(
    bridge: &ComputerUseBridge,
    ctx: &crate::agent::ToolContext,
    path: &str,
    cancellation: &CancellationToken,
) -> Result<Value, String> {
    let path = crate::tools::common::resolve_write_path(ctx, path)?;
    bridge.execute(
        Action::Screenshot { path },
        TargetRef::None,
        VerificationPolicy::None,
        SafetyClass::Reversible,
        cancellation,
    )
}

fn click(
    bridge: &ComputerUseBridge,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, String> {
    let button = match args.get("button").and_then(Value::as_str).unwrap_or("left") {
        "right" => MouseButton::Right,
        "middle" => MouseButton::Middle,
        "left" => MouseButton::Left,
        _ => return Err("button must be left, right, or middle".to_string()),
    };
    let count = u32::try_from(args.get("count").and_then(Value::as_u64).unwrap_or(1))
        .unwrap_or(u32::MAX)
        .max(1);
    if args.get("coords").is_some() || args.get("x").is_some() || args.get("y").is_some() {
        let (x, y) = coordinates(args)?;
        let observation = bridge
            .observer()
            .observe_coordinates()
            .map_err(|error| error.to_string())?;
        let display = observation
            .displays
            .iter()
            .find(|display| {
                x >= display.x
                    && y >= display.y
                    && x < display.x.saturating_add(display.width)
                    && y < display.y.saturating_add(display.height)
            })
            .ok_or_else(|| "coordinates are outside the observed displays".to_string())?;
        return bridge.execute(
            Action::Click {
                button,
                count,
                allow_coordinate_fallback: false,
            },
            TargetRef::Coordinates {
                x,
                y,
                display_id: display.display_id.clone(),
                display_geometry_hash: observation.display_geometry_hash,
                snapshot_id: observation.snapshot_id,
                snapshot_content_hash: observation.snapshot_content_hash,
            },
            VerificationPolicy::None,
            SafetyClass::Reversible,
            cancellation,
        );
    }
    if button != MouseButton::Left || count != 1 {
        return Err("semantic clicks support one left-button invocation".to_string());
    }
    let observation = bridge
        .observation()
        .ok_or_else(|| "run cu_see before selecting a semantic element".to_string())?;
    if let Some(snapshot) = args.get("snapshot").and_then(Value::as_str) {
        if snapshot != observation.observation_id {
            return Err("semantic snapshot is stale".to_string());
        }
    }
    let element = if let Some(index) = args.get("index").and_then(Value::as_u64) {
        observation.elements.get(index as usize)
    } else if let Some(query) = args.get("on").and_then(Value::as_str) {
        observation.elements.iter().find(|element| {
            element.tag == query
                || element.role.eq_ignore_ascii_case(query)
                || element
                    .name
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(query))
        })
    } else {
        None
    }
    .ok_or_else(|| "semantic element not found".to_string())?;
    let target = observation
        .target(&element.tag)
        .map_err(|error| error.to_string())?;
    bridge.execute(
        Action::Invoke,
        TargetRef::Element { target },
        VerificationPolicy::None,
        SafetyClass::Reversible,
        cancellation,
    )
}

fn window(
    bridge: &ComputerUseBridge,
    args: &Value,
    cancellation: &CancellationToken,
    deadline: i64,
) -> Result<Value, String> {
    match args.get("action").and_then(Value::as_str).unwrap_or("list") {
        "list" => bridge
            .observer()
            .list_windows(cancellation, deadline)
            .map_err(|error| error.to_string()),
        action => bridge.execute(
            Action::Window {
                operation: match action {
                    "focus" => WindowOperation::Focus,
                    "close" => WindowOperation::Close,
                    "minimize" => WindowOperation::Minimize,
                    _ => return Err("window action must be list, focus, close, or minimize".into()),
                },
                app: args.get("app").and_then(Value::as_str).map(str::to_string),
                title: args
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            },
            TargetRef::None,
            VerificationPolicy::None,
            SafetyClass::Reversible,
            cancellation,
        ),
    }
}

fn application(
    bridge: &ComputerUseBridge,
    args: &Value,
    cancellation: &CancellationToken,
    deadline: i64,
) -> Result<Value, String> {
    match args.get("action").and_then(Value::as_str).unwrap_or("list") {
        "list" => bridge
            .observer()
            .list_applications(cancellation, deadline)
            .map_err(|error| error.to_string()),
        action => bridge.execute(
            Action::Application {
                operation: match action {
                    "launch" => ApplicationOperation::Launch,
                    "switch" => ApplicationOperation::Switch,
                    "quit" => ApplicationOperation::Quit,
                    _ => return Err("app action must be list, launch, switch, or quit".into()),
                },
                name: required_string(args, "name")?.to_string(),
            },
            TargetRef::None,
            VerificationPolicy::None,
            SafetyClass::External,
            cancellation,
        ),
    }
}

fn list(
    bridge: &ComputerUseBridge,
    args: &Value,
    cancellation: &CancellationToken,
    deadline: i64,
) -> Result<Value, String> {
    match args.get("what").and_then(Value::as_str).unwrap_or("apps") {
        "apps" => bridge
            .observer()
            .list_applications(cancellation, deadline)
            .map_err(|error| error.to_string()),
        "windows" => bridge
            .observer()
            .list_windows(cancellation, deadline)
            .map_err(|error| error.to_string()),
        "screens" => serde_json::to_value(
            bridge
                .observer()
                .observe_coordinates()
                .map_err(|error| error.to_string())?
                .displays,
        )
        .map_err(|error| error.to_string()),
        _ => Err("what must be apps, windows, or screens".to_string()),
    }
}

fn clipboard(
    bridge: &ComputerUseBridge,
    args: &Value,
    cancellation: &CancellationToken,
    deadline: i64,
) -> Result<Value, String> {
    match required_string(args, "action")? {
        "read" => bridge
            .observer()
            .clipboard_read(cancellation, deadline)
            .map(Value::String)
            .map_err(|error| error.to_string()),
        "write" => bridge.execute(
            Action::ClipboardWrite {
                text: required_string(args, "text")?.to_string(),
            },
            TargetRef::None,
            VerificationPolicy::None,
            SafetyClass::External,
            cancellation,
        ),
        _ => Err("clipboard action must be read or write".to_string()),
    }
}

fn coordinates(args: &Value) -> Result<(i64, i64), String> {
    if let Some(value) = args.get("coords").and_then(Value::as_str) {
        let (x, y) = value
            .split_once(',')
            .ok_or_else(|| "coords must be x,y".to_string())?;
        return Ok((
            x.trim().parse().map_err(|_| "invalid x coordinate")?,
            y.trim().parse().map_err(|_| "invalid y coordinate")?,
        ));
    }
    Ok((
        args.get("x").and_then(Value::as_i64).unwrap_or(0),
        args.get("y").and_then(Value::as_i64).unwrap_or(0),
    ))
}

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{key} required"))
}

fn direction(value: Option<&str>) -> Direction {
    match value.unwrap_or("down") {
        "up" => Direction::Up,
        "left" => Direction::Left,
        "right" => Direction::Right,
        _ => Direction::Down,
    }
}

fn deadline() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| {
            i64::try_from(duration.as_millis())
                .unwrap_or(i64::MAX)
                .saturating_add(30_000)
        })
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ToolRegistry;

    #[test]
    fn parses_explicit_coordinates() {
        assert_eq!(
            coordinates(&serde_json::json!({"coords": "-12,34"})).unwrap(),
            (-12, 34)
        );
        assert!(coordinates(&serde_json::json!({"coords": "12"})).is_err());
    }

    #[test]
    fn rejects_empty_required_values() {
        assert!(required_string(&serde_json::json!({"text": ""}), "text").is_err());
    }

    #[test]
    fn test_register_tools() {
        let mut registry = ToolRegistry::new();
        register_tools(&mut registry);

        assert_eq!(registry.count(), 13);

        let expected_tools = vec![
            "cu_call",
            "cu_see",
            "cu_image",
            "cu_click",
            "cu_type",
            "cu_hotkey",
            "cu_scroll",
            "cu_window",
            "cu_app",
            "cu_list",
            "cu_open",
            "cu_clipboard",
            "cu_doctor",
        ];

        let definitions = registry.definitions();
        for expected in expected_tools {
            let found = definitions
                .iter()
                .any(|d| d["name"].as_str() == Some(expected));
            assert!(found, "Tool {} not registered", expected);
        }
    }
}
