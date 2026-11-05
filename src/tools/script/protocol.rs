use serde_json::Value;
use std::io::{Read, Write};
pub const MAX_FRAME: usize = 2 * 1024 * 1024;
pub fn read_frame(reader: &mut impl Read) -> Result<Value, String> {
    let mut size = [0; 4];
    reader.read_exact(&mut size).map_err(|e| e.to_string())?;
    let size = u32::from_be_bytes(size) as usize;
    if size > MAX_FRAME {
        return Err("script frame exceeds limit".into());
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
pub fn write_frame(writer: &mut impl Write, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FRAME {
        return Err("script frame exceeds limit".into());
    }
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .and_then(|()| writer.write_all(&bytes))
        .and_then(|()| writer.flush())
        .map_err(|e| e.to_string())
}
