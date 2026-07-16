//! Optimized SSE (Server-Sent Events) parser.
//!
//! Implements the [SSE wire format](https://html.spec.whatwg.org/multipage/server-sent-events.html)
//! with event-type interning, BOM stripping, per-event data caps, and an
//! optional async stream adapter gated behind the `providers` feature.

use std::borrow::Cow;

#[cfg(feature = "providers")]
use futures::StreamExt;

/// Maximum total bytes of accumulated event data before a parse is rejected.
pub const MAX_EVENT_DATA_BYTES: usize = 100 * 1024 * 1024;

/// UTF-8 BOM byte sequence, stripped from the start of the stream.
const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// Errors produced while parsing or transporting SSE.
#[derive(Debug, thiserror::Error)]
pub enum SseError {
    #[error("sse parse error: {0}")]
    ParseError(String),
    #[error("sse data limit exceeded ({MAX_EVENT_DATA_BYTES} bytes)")]
    DataLimitExceeded,
    #[error("sse stream error: {0}")]
    StreamError(String),
}

/// A single parsed SSE event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// Event type. Defaults to `"message"`. Common types are interned as
    /// `Cow::Borrowed` to avoid per-event allocation.
    pub event: Cow<'static, str>,
    /// Concatenated `data:` lines separated by `\n`.
    pub data: String,
    /// Last event ID seen on this event.
    pub id: Option<String>,
    /// Retry interval in milliseconds, if `retry:` was set.
    pub retry: Option<u64>,
}

impl SseEvent {
    fn message() -> Self {
        Self {
            event: Cow::Borrowed("message"),
            data: String::new(),
            id: None,
            retry: None,
        }
    }
}

/// Intern a field name into a `Cow<'static, str>` when it matches a common
/// SSE event type, otherwise allocate.
fn intern_event_type(name: &str) -> Cow<'static, str> {
    match name {
        "data" => Cow::Borrowed("data"),
        "event" => Cow::Borrowed("event"),
        "id" => Cow::Borrowed("id"),
        "retry" => Cow::Borrowed("retry"),
        "ping" => Cow::Borrowed("ping"),
        other => Cow::Owned(other.to_string()),
    }
}

/// Incremental SSE parser.
///
/// Feed byte chunks as they arrive from the network; completed events are
/// returned from [`feed`]. Call [`finish`] to flush any trailing event that
/// was not terminated by a blank line.
///
/// [`feed`]: SseParser::feed
/// [`finish`]: SseParser::finish
#[derive(Debug)]
pub struct SseParser {
    buf: Vec<u8>,
    /// Byte offset in `buf` up to which lines have already been scanned.
    scan: usize,
    /// Whether the BOM has been checked/stripped yet.
    bom_checked: bool,
    /// Accumulator for the event currently being built.
    current: SseEvent,
    /// Whether `current` has received any non-comment field yet.
    has_fields: bool,
    /// Last event ID carried across events per the spec.
    last_event_id: Option<String>,
}

impl Default for SseParser {
    fn default() -> Self {
        Self {
            buf: Vec::new(),
            scan: 0,
            bom_checked: false,
            current: SseEvent::message(),
            has_fields: false,
            last_event_id: None,
        }
    }
}

impl SseParser {
    /// Create a new parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes and return any completed events.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        if chunk.is_empty() {
            return Vec::new();
        }
        self.buf.extend_from_slice(chunk);
        self.process()
    }

    /// Flush any buffered partial event as a final event, if it had data.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if self.scan < self.buf.len() {
            let remaining = self.buf[self.scan..].to_vec();
            self.scan = self.buf.len();
            if let Ok(line) = std::str::from_utf8(&remaining) {
                self.apply_line(line, &mut Vec::new());
            }
        }
        if self.has_fields && (!self.current.data.is_empty() || self.current.id.is_some()) {
            let ev = std::mem::replace(&mut self.current, SseEvent::message());
            self.has_fields = false;
            self.buf.clear();
            self.scan = 0;
            Some(ev)
        } else {
            None
        }
    }

    fn process(&mut self) -> Vec<SseEvent> {
        if !self.bom_checked {
            self.bom_checked = true;
            if self.buf.starts_with(&UTF8_BOM) {
                self.buf.drain(..UTF8_BOM.len());
            }
        }

        let mut events = Vec::new();
        loop {
            if self.scan >= self.buf.len() {
                break;
            }
            let (line, consumed, crlf) = {
                let remaining = &self.buf[self.scan..];
                let pos = match remaining.iter().position(|&b| b == b'\r' || b == b'\n') {
                    Some(p) => p,
                    None => break,
                };
                let line_bytes = &remaining[..pos];
                let line = match std::str::from_utf8(line_bytes) {
                    Ok(s) => s.to_string(),
                    Err(e) => {
                        events.push(SseEvent {
                            event: Cow::Borrowed("error"),
                            data: format!("invalid utf8: {e}"),
                            id: None,
                            retry: None,
                        });
                        self.buf.clear();
                        self.scan = 0;
                        self.has_fields = false;
                        self.current = SseEvent::message();
                        return events;
                    }
                };
                let crlf =
                    remaining.get(pos) == Some(&b'\r') && remaining.get(pos + 1) == Some(&b'\n');
                (line, pos + 1, crlf)
            };

            self.apply_line(&line, &mut events);

            self.scan += consumed;
            if crlf {
                self.scan += 1;
            }
        }

        self.compact();
        events
    }

    fn apply_line(&mut self, line: &str, events: &mut Vec<SseEvent>) {
        if line.is_empty() {
            if self.has_fields {
                let mut ev = std::mem::replace(&mut self.current, SseEvent::message());
                self.has_fields = false;
                if let Some(id) = &ev.id {
                    self.last_event_id = Some(id.clone());
                } else if let Some(id) = &self.last_event_id {
                    ev.id = Some(id.clone());
                }
                if !ev.data.is_empty() || ev.id.is_some() || ev.event != "message" {
                    events.push(ev);
                }
            }
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.find(':') {
            Some(idx) => {
                let field = &line[..idx];
                let mut value = &line[idx + 1..];
                if let Some(v) = value.strip_prefix(' ') {
                    value = v;
                }
                (field, value)
            }
            None => (line, ""),
        };
        if let Some(err) = self.apply_field(field, value) {
            events.push(err);
        }
    }

    fn apply_field(&mut self, field: &str, value: &str) -> Option<SseEvent> {
        self.has_fields = true;
        match field {
            "event" => {
                self.current.event = intern_event_type(value);
            }
            "data" => {
                if self.current.data.len() + value.len() + 1 > MAX_EVENT_DATA_BYTES {
                    self.current.data.clear();
                    self.has_fields = false;
                    self.current = SseEvent::message();
                    return Some(SseEvent {
                        event: Cow::Borrowed("error"),
                        data: "data limit exceeded".to_string(),
                        id: None,
                        retry: None,
                    });
                }
                if !self.current.data.is_empty() {
                    self.current.data.push('\n');
                }
                self.current.data.push_str(value);
            }
            "id" => {
                if !value.contains('\n') {
                    self.current.id = Some(value.to_string());
                }
            }
            "retry" => {
                if let Ok(ms) = value.parse::<u64>() {
                    self.current.retry = Some(ms);
                }
            }
            _ => {}
        }
        None
    }

    fn compact(&mut self) {
        if self.scan > 0 && self.scan == self.buf.len() {
            self.buf.clear();
            self.scan = 0;
        } else if self.scan > 0 && self.scan >= self.buf.len() / 2 {
            self.buf.drain(..self.scan);
            self.scan = 0;
        }
    }
}

/// Parse a byte stream into SSE events.
///
/// Requires the `providers` feature (uses `futures`). Accepts any stream
/// yielding byte buffers (e.g. `reqwest`'s `bytes_stream`, which yields
/// `bytes::Bytes`).
#[cfg(feature = "providers")]
pub async fn parse_stream<S, B, E>(mut stream: S) -> Vec<Result<SseEvent, SseError>>
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    let mut parser = SseParser::new();
    let mut out = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(buf) => {
                for ev in parser.feed(buf.as_ref()) {
                    if ev.event == "error" && ev.data == "data limit exceeded" {
                        out.push(Err(SseError::DataLimitExceeded));
                    } else {
                        out.push(Ok(ev));
                    }
                }
            }
            Err(e) => out.push(Err(SseError::StreamError(e.to_string()))),
        }
    }
    if let Some(ev) = parser.finish() {
        out.push(Ok(ev));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_complete_data_event() {
        let mut p = SseParser::new();
        let events = p.feed(b"data: hello\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
        assert_eq!(events[0].event, Cow::Borrowed("message"));
    }

    #[test]
    fn parses_multiple_events_one_chunk() {
        let mut p = SseParser::new();
        let events = p.feed(b"data: a\n\ndata: b\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "a");
        assert_eq!(events[1].data, "b");
    }

    #[test]
    fn parses_event_split_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: hel").is_empty());
        assert!(p.feed(b"lo\n\n").len() == 1);
        let events = p.feed(b"data: hel");
        assert!(events.is_empty());
        let events = p.feed(b"lo\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn strips_utf8_bom() {
        let mut p = SseParser::new();
        let events = p.feed(b"\xEF\xBB\xBFdata: hi\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hi");
    }

    #[test]
    fn rejects_data_over_cap() {
        let mut p = SseParser::new();
        let big = "x".repeat(MAX_EVENT_DATA_BYTES + 1);
        let chunk = format!("data: {big}\n\n");
        let events = p.feed(chunk.as_bytes());
        assert!(events.iter().any(|e| e.event == "error"));
    }

    #[test]
    fn interns_common_event_types() {
        let mut p = SseParser::new();
        let events = p.feed(b"event: ping\ndata: 1\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, Cow::Borrowed("ping"));
        assert!(matches!(events[0].event, Cow::Borrowed("ping")));

        let events = p.feed(b"event: custom\ndata: 2\n\n");
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].event, Cow::Owned(_)));
    }

    #[test]
    fn parses_id_and_retry_fields() {
        let mut p = SseParser::new();
        let events = p.feed(b"id: 42\ndata: x\nretry: 5000\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("42"));
        assert_eq!(events[0].retry, Some(5000));
    }

    #[test]
    fn handles_crlf_line_endings() {
        let mut p = SseParser::new();
        let events = p.feed(b"data: hello\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn concatenates_multiple_data_lines() {
        let mut p = SseParser::new();
        let events = p.feed(b"data: line1\ndata: line2\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2");
    }

    #[test]
    fn ignores_comments() {
        let mut p = SseParser::new();
        let events = p.feed(b": a comment\ndata: real\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "real");
    }

    #[test]
    fn finish_flushes_partial_event() {
        let mut p = SseParser::new();
        p.feed(b"data: tail");
        let ev = p.finish();
        assert_eq!(ev.unwrap().data, "tail");
    }
}
