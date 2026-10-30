use super::protocol::{read_frame, write_frame};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
const MAX_OUTPUT: usize = 32 * 1024;
/// VM instructions between wall-clock checks in the debug hook. Count hooks
/// force LuaJIT into the interpreter, so `while true do end` is caught too.
const HOOK_INSTRUCTIONS: u32 = 50_000;

/// Explicitly enumerated host-safe std libs. Do NOT use `StdLib::ALL_SAFE`:
/// on the luajit backend it is every flag below FFI/DEBUG, which silently
/// includes `io`, `os`, `package`, and `jit`. `Utf8` does not exist on luajit.
fn safe_libs() -> mlua::StdLib {
    mlua::StdLib::STRING | mlua::StdLib::TABLE | mlua::StdLib::MATH | mlua::StdLib::BIT
}

/// mlua's `new_with` always opens the base library (it is all-or-nothing), so
/// the needed base functions (pcall, pairs, ipairs, tostring, tonumber, type,
/// select, error, assert, setmetatable, rawget/rawset, unpack) come from base
/// and we strip the base functions that reach the host instead: file loading
/// (`dofile`/`loadfile`), arbitrary chunk + bytecode loading (`load`,
/// `loadstring`), and the module loader (`require`, `module`). `os`, `io`,
/// `debug`, `package`, and `ffi` are separate libraries never loaded here.
const STRIP_ESCAPE_HATCHES: &str = r#"
for _, name in ipairs({'dofile', 'loadfile', 'load', 'loadstring', 'require', 'module'}) do
    _G[name] = nil
end
"#;

struct PrintBuffer(std::sync::Mutex<Vec<String>>);

#[derive(Clone)]
struct Budget {
    deadline: Instant,
}
impl Budget {
    fn check(&self) -> mlua::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(mlua::Error::runtime("script timed out (wall-clock cap)"));
        }
        Ok(())
    }
}
// Protected Lua calls may catch ordinary errors, but never the execution deadline.
fn install_protected_calls(lua: &mlua::Lua, budget: &Budget) -> mlua::Result<()> {
    for name in ["pcall", "xpcall"] {
        let original: mlua::Function = lua.globals().get(name)?;
        let budget = budget.clone();
        let protected = lua.create_function(move |_, args: mlua::MultiValue| {
            budget.check()?;
            let result = original.call::<mlua::MultiValue>(args);
            // Raise outside the native protected call; enclosing wrappers recheck too.
            budget.check()?;
            result
        })?;
        lua.globals().set(name, protected)?;
    }
    Ok(())
}

fn run_owned_lua(code: String, duration: Duration) -> Result<String, String> {
    let budget = Budget {
        deadline: Instant::now() + duration,
    };
    let lua = mlua::Lua::new_with(safe_libs(), mlua::LuaOptions::new())
        .map_err(|e| format!("lua init: {e}"))?;
    // LuaJIT cannot enforce allocator limits; do not claim a memory cap.
    lua.load(STRIP_ESCAPE_HATCHES)
        .set_name("=sandbox-bootstrap")
        .exec()
        .map_err(|e| format!("sandbox bootstrap failed: {e}"))?;
    install_protected_calls(&lua, &budget).map_err(|e| e.to_string())?;
    let hook_budget = budget.clone();
    lua.set_hook(
        mlua::HookTriggers::new().every_nth_instruction(HOOK_INSTRUCTIONS),
        move |_, _| {
            hook_budget.check()?;
            Ok(mlua::VmState::Continue)
        },
    );
    install_bridges(&lua, &budget).map_err(|e| format!("bridge setup failed: {e}"))?;
    let result: mlua::Value = lua
        .load(&code)
        .set_name("script")
        .eval()
        .map_err(|e| e.to_string())?;
    budget.check().map_err(|e| e.to_string())?;
    let out = value_to_string(&result, &budget).map_err(|e| e.to_string())?;
    let printed = lua
        .app_data_ref::<PrintBuffer>()
        .map(|b| b.0.lock().map(|v| v.join("\n")).unwrap_or_default())
        .unwrap_or_default();
    let mut body = printed;
    if !out.is_empty() && out != "nil" {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str("result: ");
        body.push_str(&out);
    }
    if body.len() > MAX_OUTPUT {
        return Err("script output exceeds limit".into());
    }
    if body.is_empty() {
        body.push_str("(no output — print() or return a value)");
    }
    budget.check().map_err(|e| e.to_string())?;
    Ok(body)
}

fn install_bridges(lua: &mlua::Lua, budget: &Budget) -> mlua::Result<()> {
    lua.set_app_data(PrintBuffer(std::sync::Mutex::new(Vec::new())));
    let print_budget = budget.clone();
    lua.globals().set(
        "print",
        lua.create_function(move |lua, args: mlua::MultiValue| {
            let mut parts = Vec::new();
            let mut size = 0;
            for value in args {
                let text = value_to_string(&value, &print_budget)?;
                size += text.len() + 1;
                if size > MAX_OUTPUT {
                    return Err(mlua::Error::runtime("script output exceeds limit"));
                }
                parts.push(text);
            }
            if let Some(buffer) = lua.app_data_ref::<PrintBuffer>() {
                let mut lines = buffer
                    .0
                    .lock()
                    .map_err(|_| mlua::Error::runtime("print buffer unavailable"))?;
                if lines.iter().map(|line| line.len() + 1).sum::<usize>() + size > MAX_OUTPUT {
                    return Err(mlua::Error::runtime("script output exceeds limit"));
                }
                lines.push(parts.join("\t"));
            }
            Ok(())
        })?,
    )?;
    let sequence = std::rc::Rc::new(std::cell::Cell::new(0u64));
    for op in ["read", "list", "grep"] {
        let sequence = sequence.clone();
        let budget = budget.clone();
        lua.globals().set(
            op,
            lua.create_function(move |lua, args: mlua::MultiValue| {
                budget.check()?;
                let id = sequence.get() + 1;
                sequence.set(id);
                let values = args
                    .into_iter()
                    .map(|v| lua_value_to_json(&v, &budget))
                    .collect::<mlua::Result<Vec<_>>>()?;
                write_frame(
                    &mut std::io::stdout().lock(),
                    &json!({"version":1,"kind":"bridge","id":id,"op":op,"args":values}),
                )
                .map_err(mlua::Error::external)?;
                let response =
                    read_frame(&mut std::io::stdin().lock()).map_err(mlua::Error::external)?;
                budget.check()?;
                if response["version"] != 1
                    || response["kind"] != "bridge_result"
                    || response["id"] != id
                {
                    return Err(mlua::Error::runtime("invalid bridge response"));
                }
                if let Some(error) = response["error"].as_str() {
                    return Err(mlua::Error::runtime(error.to_string()));
                }
                json_to_lua(lua, &response["value"], 0)
            })?,
        )?;
    }
    let encode_budget = budget.clone();
    let json_table = lua.create_table()?;
    json_table.set(
        "encode",
        lua.create_function(move |_, value: mlua::Value| {
            let text = serde_json::to_string(&lua_value_to_json(&value, &encode_budget)?)
                .map_err(mlua::Error::external)?;
            if text.len() > MAX_OUTPUT {
                return Err(mlua::Error::runtime("JSON output exceeds limit"));
            }
            Ok(text)
        })?,
    )?;
    let decode_budget = budget.clone();
    json_table.set(
        "decode",
        lua.create_function(move |lua, text: String| {
            decode_budget.check()?;
            if text.len() > MAX_OUTPUT {
                return Err(mlua::Error::runtime("JSON input exceeds limit"));
            }
            let value: Value = serde_json::from_str(&text).map_err(mlua::Error::external)?;
            json_to_lua(lua, &value, 0)
        })?,
    )?;
    lua.globals().set("json", json_table)?;
    Ok(())
}
fn value_to_string(val: &mlua::Value, budget: &Budget) -> mlua::Result<String> {
    budget.check()?;
    let text = match val {
        mlua::Value::Nil => "nil".into(),
        mlua::Value::Boolean(b) => b.to_string(),
        mlua::Value::Integer(i) => i.to_string(),
        mlua::Value::Number(f) => f.to_string(),
        mlua::Value::String(s) => s.to_string_lossy(),
        other => serde_json::to_string(&lua_value_to_json(other, budget)?)
            .map_err(mlua::Error::external)?,
    };
    if text.len() > MAX_OUTPUT {
        return Err(mlua::Error::runtime("script output exceeds limit"));
    }
    Ok(text)
}

fn lua_value_to_json(val: &mlua::Value, budget: &Budget) -> mlua::Result<serde_json::Value> {
    fn convert(
        val: &mlua::Value,
        budget: &Budget,
        depth: usize,
        items: &mut usize,
        bytes: &mut usize,
        seen: &mut std::collections::HashSet<usize>,
    ) -> mlua::Result<serde_json::Value> {
        budget.check()?;
        *items += 1;
        if depth > 32 || *items > 4096 {
            return Err(mlua::Error::runtime("table conversion exceeds limit"));
        }
        Ok(match val {
            mlua::Value::Nil => serde_json::Value::Null,
            mlua::Value::Boolean(b) => serde_json::json!(b),
            mlua::Value::Integer(i) => serde_json::json!(i),
            mlua::Value::Number(f) => serde_json::json!(f),
            mlua::Value::String(s) => {
                *bytes += s.as_bytes().len();
                if *bytes > MAX_OUTPUT {
                    return Err(mlua::Error::runtime("string exceeds limit"));
                }
                serde_json::json!(s.to_string_lossy())
            }
            mlua::Value::Table(table) => {
                let pointer = table.to_pointer() as usize;
                if !seen.insert(pointer) {
                    return Err(mlua::Error::runtime("cyclic table"));
                }
                let mut map = serde_json::Map::new();
                let mut sequence = std::collections::BTreeMap::new();
                let mut array = true;
                for pair in table.clone().pairs::<mlua::Value, mlua::Value>() {
                    budget.check()?;
                    let (key, value) = pair?;
                    let converted = convert(&value, budget, depth + 1, items, bytes, seen)?;
                    if let mlua::Value::Integer(index) = &key {
                        if *index > 0 {
                            sequence.insert(*index, converted.clone());
                        } else {
                            array = false;
                        }
                    } else {
                        array = false;
                    }
                    let key = match key {
                        mlua::Value::String(key) => {
                            *bytes += key.as_bytes().len();
                            if *bytes > MAX_OUTPUT {
                                return Err(mlua::Error::runtime("key exceeds limit"));
                            }
                            key.to_string_lossy()
                        }
                        mlua::Value::Integer(key) => key.to_string(),
                        _ => return Err(mlua::Error::runtime("unsupported table key")),
                    };
                    map.insert(key, converted);
                }
                seen.remove(&pointer);
                if array
                    && !sequence.is_empty()
                    && sequence.keys().copied().eq(1..=sequence.len() as i64)
                {
                    serde_json::Value::Array(sequence.into_values().collect())
                } else {
                    serde_json::Value::Object(map)
                }
            }
            other => serde_json::json!(format!("{other:?}")),
        })
    }
    convert(
        val,
        budget,
        0,
        &mut 0,
        &mut 0,
        &mut std::collections::HashSet::new(),
    )
}

fn json_to_lua(
    lua: &mlua::Lua,
    json: &serde_json::Value,
    depth: usize,
) -> Result<mlua::Value, mlua::Error> {
    if depth > 32 {
        return Err(mlua::Error::runtime("JSON depth exceeds limit"));
    }
    Ok(match json {
        serde_json::Value::Null => mlua::Value::Nil,
        serde_json::Value::Bool(b) => mlua::Value::Boolean(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => mlua::Value::Integer(i),
            None => mlua::Value::Number(n.as_f64().unwrap_or(0.0)),
        },
        serde_json::Value::String(s) => mlua::Value::String(lua.create_string(s)?),
        serde_json::Value::Array(items) => {
            let t = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                t.set(i + 1, json_to_lua(lua, item, depth + 1)?)?;
            }
            mlua::Value::Table(t)
        }
        serde_json::Value::Object(map) => {
            let t = lua.create_table()?;
            for (k, v) in map {
                t.set(k.as_str(), json_to_lua(lua, v, depth + 1)?)?;
            }
            mlua::Value::Table(t)
        }
    })
}

pub fn main() -> Result<(), String> {
    write_frame(
        &mut std::io::stdout().lock(),
        &json!({"version":1,"kind":"hello"}),
    )?;
    let start = read_frame(&mut std::io::stdin().lock())?;
    if start["version"] != 1 || start["kind"] != "start" {
        return Err("invalid start frame".into());
    }
    let code = start["code"].as_str().ok_or("code required")?;
    let timeout = start["timeout_ms"].as_u64().ok_or("timeout required")?;
    if code.len() > 65536 || !(1000..=30000).contains(&timeout) {
        return Err("invalid script limits".into());
    }
    // Native Lua C operations do not run instruction hooks. The disposable helper
    // must also stop if its parent disappears while such an operation is active.
    std::thread::Builder::new()
        .name("rx4-script-deadline".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(timeout));
            std::process::exit(124);
        })
        .map_err(|error| error.to_string())?;
    let result = run_owned_lua(code.into(), Duration::from_millis(timeout));
    write_frame(
        &mut std::io::stdout().lock(),
        &match result {
            Ok(output) => json!({"version":1,"kind":"result","output":output}),
            Err(error) => json!({"version":1,"kind":"result","error":error}),
        },
    )
}
