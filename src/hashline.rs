//! Hashline edit protocol: tagged reads and fail-closed PUT/CUT/MV/REM.
//!
//! Hosts call this API instead of owning their own line-edit dialect.
//! A read is `[path#TAG]` plus `N:line`. Edits name a tag and speak PUT, CUT,
//! MV, and REM. Stale tags, elided lines, unseen lines, and no-ops fail closed.

use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;

/// How aggressively to parse an edit script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    /// Verbs uppercase, `N` or `N-M`, colon required on PUT.
    Strict,
    /// Lowercase verbs, optional colon, optional quotes, extra whitespace.
    Sloppy,
}

/// Model families that may use sloppy parse as a fallback after strict fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFamily {
    Strict,
    Sloppy,
}

/// Visible (non-elided) line numbers from a tagged read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleSet {
    all: bool,
    lines: BTreeSet<usize>,
}

impl VisibleSet {
    pub fn all_lines() -> Self {
        Self {
            all: true,
            lines: BTreeSet::new(),
        }
    }

    pub fn from_lines(lines: impl IntoIterator<Item = usize>) -> Self {
        Self {
            all: false,
            lines: lines.into_iter().collect(),
        }
    }

    pub fn allows(&self, line: usize) -> bool {
        self.all || self.lines.contains(&line)
    }

    pub fn allows_range(&self, start: usize, end: usize) -> bool {
        (start..=end).all(|n| self.allows(n))
    }
}

/// Result of formatting a file for the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedRead {
    pub path: String,
    pub tag: String,
    pub text: String,
    pub visible: VisibleSet,
    pub line_count: usize,
}

/// Limits for eliding large files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOptions {
    /// When `Some` and the file has more lines, keep `head` + `tail` and elide the rest.
    pub max_visible: Option<usize>,
    pub head: usize,
    pub tail: usize,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            max_visible: Some(400),
            head: 200,
            tail: 80,
        }
    }
}

/// Fail-closed hashline errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashlineError {
    StaleTag { expected: String, actual: String },
    UnseenLine { line: usize },
    ElidedLine { line: usize },
    Noop,
    Parse(String),
    Ambiguous(String),
    EmptyScript,
}

impl fmt::Display for HashlineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleTag { expected, actual } => {
                write!(f, "stale tag: expected {expected}, file is {actual}")
            }
            Self::UnseenLine { line } => write!(f, "unseen line {line}"),
            Self::ElidedLine { line } => write!(f, "elided line {line}"),
            Self::Noop => write!(f, "no-op edit"),
            Self::Parse(m) => write!(f, "parse error: {m}"),
            Self::Ambiguous(m) => write!(f, "ambiguous edit: {m}"),
            Self::EmptyScript => write!(f, "empty edit script"),
        }
    }
}

impl std::error::Error for HashlineError {}

/// Short stable content tag (16 hex chars of SHA-256).
pub fn tag_for(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let out = hasher.finalize();
    out[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Format a tagged, numbered read. Elided ranges are not in [`TaggedRead::visible`].
pub fn format_read(path: &str, content: &str, opts: ReadOptions) -> TaggedRead {
    let tag = tag_for(content);
    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();
    let mut out = format!("[{path}#{tag}]\n");
    let (visible, show): (VisibleSet, Vec<usize>) = match opts.max_visible {
        Some(max) if n > max && opts.head + opts.tail < n => {
            let head = opts.head.min(n);
            let tail = opts.tail.min(n.saturating_sub(head));
            let tail_start = n - tail + 1;
            let mut vis = BTreeSet::new();
            let mut order = Vec::new();
            for i in 1..=head {
                vis.insert(i);
                order.push(i);
            }
            for i in tail_start..=n {
                vis.insert(i);
                order.push(i);
            }
            (VisibleSet::from_lines(vis), order)
        }
        _ => (VisibleSet::all_lines(), (1..=n).collect()),
    };

    let mut last = 0usize;
    for i in &show {
        if last > 0 && *i > last + 1 {
            out.push_str(&format!("... [elided {}-{}] ...\n", last + 1, i - 1));
        }
        let text = lines[*i - 1];
        out.push_str(&format!("{i}:{text}\n"));
        last = *i;
    }
    if n == 0 {
        out.push_str("(empty file)\n");
    }

    TaggedRead {
        path: path.to_string(),
        tag,
        text: out,
        visible,
        line_count: n,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Put {
        start: usize,
        end: usize,
        lines: Vec<String>,
    },
    Cut {
        start: usize,
        end: usize,
    },
    Mv {
        start: usize,
        end: usize,
        dest: usize,
    },
    Rem,
}

/// Apply a hashline script to `content`. Fails closed on stale tag, unseen or
/// elided lines, parse failure, and no-ops.
pub fn apply(
    content: &str,
    expected_tag: &str,
    script: &str,
    visible: &VisibleSet,
    family: ModelFamily,
) -> Result<String, HashlineError> {
    let actual = tag_for(content);
    if actual != expected_tag {
        return Err(HashlineError::StaleTag {
            expected: expected_tag.to_string(),
            actual,
        });
    }
    let ops = parse_script(script, family)?;
    if ops.iter().all(|o| matches!(o, Op::Rem)) {
        return Err(HashlineError::Noop);
    }
    let (mut lines, ended_nl) = split_keep(content);
    let total = lines.len();
    for op in &ops {
        match op {
            Op::Put { start, end, .. } | Op::Cut { start, end } | Op::Mv { start, end, .. } => {
                check_range(*start, *end, total, visible)?;
            }
            Op::Rem => {}
        }
        if let Op::Mv { dest, .. } = op {
            if *dest == 0 || *dest > total + 1 {
                return Err(HashlineError::UnseenLine { line: *dest });
            }
        }
    }
    for op in ops {
        apply_op(&mut lines, op)?;
    }
    let mut next = lines.join("\n");
    if ended_nl || content.is_empty() {
        if !next.ends_with('\n') {
            next.push('\n');
        }
    }
    if content.is_empty() && next == "\n" {
        next.clear();
    }
    if next == content {
        return Err(HashlineError::Noop);
    }
    Ok(next)
}

fn check_range(
    start: usize,
    end: usize,
    total: usize,
    visible: &VisibleSet,
) -> Result<(), HashlineError> {
    if start == 0 || end == 0 || start > end {
        return Err(HashlineError::Parse(format!("bad range {start}-{end}")));
    }
    if end > total {
        return Err(HashlineError::UnseenLine { line: end });
    }
    if !visible.allows_range(start, end) {
        return Err(HashlineError::ElidedLine { line: start });
    }
    Ok(())
}

fn apply_op(lines: &mut Vec<String>, op: Op) -> Result<(), HashlineError> {
    match op {
        Op::Rem => Ok(()),
        Op::Put {
            start,
            end,
            lines: repl,
        } => {
            lines.splice(start - 1..end, repl);
            Ok(())
        }
        Op::Cut { start, end } => {
            lines.drain(start - 1..end);
            Ok(())
        }
        Op::Mv { start, end, dest } => {
            if dest >= start && dest <= end {
                return Err(HashlineError::Ambiguous(
                    "MV dest inside source range".into(),
                ));
            }
            let block: Vec<String> = lines[start - 1..end].to_vec();
            let len = end - start + 1;
            lines.drain(start - 1..end);
            let insert_at = if dest > end { dest - 1 - len } else { dest - 1 };
            if insert_at > lines.len() {
                return Err(HashlineError::UnseenLine { line: dest });
            }
            for (i, line) in block.into_iter().enumerate() {
                lines.insert(insert_at + i, line);
            }
            Ok(())
        }
    }
}

fn split_keep(content: &str) -> (Vec<String>, bool) {
    if content.is_empty() {
        return (Vec::new(), false);
    }
    let ended = content.ends_with('\n');
    (content.lines().map(str::to_string).collect(), ended)
}

fn parse_script(script: &str, family: ModelFamily) -> Result<Vec<Op>, HashlineError> {
    let trimmed = script.trim();
    if trimmed.is_empty() {
        return Err(HashlineError::EmptyScript);
    }
    match parse_ops(trimmed, ParseMode::Strict) {
        Ok(ops) => Ok(ops),
        Err(_e) if family == ModelFamily::Sloppy => match parse_ops(trimmed, ParseMode::Sloppy) {
            Ok(ops) => Ok(ops),
            Err(HashlineError::Parse(m)) => Err(HashlineError::Ambiguous(m)),
            Err(other) => Err(other),
        },
        Err(e) => Err(e),
    }
}

fn parse_ops(script: &str, mode: ParseMode) -> Result<Vec<Op>, HashlineError> {
    let raw: Vec<&str> = script.lines().collect();
    let mut i = 0;
    let mut ops = Vec::new();
    while i < raw.len() {
        let line = raw[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        let (verb, rest) = take_verb(line, mode)?;
        match verb.as_str() {
            "REM" => {
                ops.push(Op::Rem);
                i += 1;
            }
            "CUT" => {
                let (start, end) = parse_range(rest, mode)?;
                ops.push(Op::Cut { start, end });
                i += 1;
            }
            "MV" => {
                let (start, end, dest) = parse_mv(rest, mode)?;
                ops.push(Op::Mv { start, end, dest });
                i += 1;
            }
            "PUT" => {
                let (start, end, inline) = parse_put_header(rest, mode)?;
                let mut body = Vec::new();
                if let Some(text) = inline {
                    if !text.is_empty() {
                        body.push(text);
                    }
                } else {
                    i += 1;
                    while i < raw.len() && !is_verb_line(raw[i], mode) {
                        body.push(strip_quotes(raw[i], mode));
                        i += 1;
                    }
                    i -= 1;
                }
                ops.push(Op::Put {
                    start,
                    end,
                    lines: body,
                });
                i += 1;
            }
            other => {
                return Err(HashlineError::Parse(format!("unknown verb {other}")));
            }
        }
    }
    if ops.is_empty() {
        return Err(HashlineError::EmptyScript);
    }
    Ok(ops)
}

fn take_verb(line: &str, mode: ParseMode) -> Result<(String, &str), HashlineError> {
    let line = match mode {
        ParseMode::Strict => line,
        ParseMode::Sloppy => line.trim(),
    };
    let mut parts = line.splitn(2, char::is_whitespace);
    let verb = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");
    let norm = match mode {
        ParseMode::Strict => verb.to_string(),
        ParseMode::Sloppy => verb.to_ascii_uppercase(),
    };
    if !matches!(norm.as_str(), "PUT" | "CUT" | "MV" | "REM") {
        return Err(HashlineError::Parse(format!("unknown verb {verb}")));
    }
    if mode == ParseMode::Strict && verb != norm {
        return Err(HashlineError::Parse(format!(
            "verb must be uppercase: {verb}"
        )));
    }
    Ok((norm, rest))
}

fn is_verb_line(line: &str, mode: ParseMode) -> bool {
    take_verb(line, mode).is_ok()
}

fn parse_range(rest: &str, mode: ParseMode) -> Result<(usize, usize), HashlineError> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Err(HashlineError::Parse("missing range".into()));
    }
    let token = rest.split_whitespace().next().unwrap_or("");
    parse_span(token, mode)
}

fn parse_span(token: &str, mode: ParseMode) -> Result<(usize, usize), HashlineError> {
    let token = token.trim_end_matches(':');
    if let Some((a, b)) = token.split_once('-') {
        let start = parse_num(a, mode)?;
        let end = parse_num(b, mode)?;
        if start > end {
            return Err(HashlineError::Parse(format!("bad range {token}")));
        }
        Ok((start, end))
    } else {
        let n = parse_num(token, mode)?;
        Ok((n, n))
    }
}

fn parse_num(s: &str, _mode: ParseMode) -> Result<usize, HashlineError> {
    s.trim()
        .parse()
        .map_err(|_| HashlineError::Parse(format!("not a line number: {s}")))
}

fn parse_mv(rest: &str, mode: ParseMode) -> Result<(usize, usize, usize), HashlineError> {
    let rest = rest.trim();
    let rest = if mode == ParseMode::Sloppy {
        rest.replace(" to ", " ").replace(" TO ", " ")
    } else {
        rest.to_string()
    };
    let bits: Vec<&str> = rest.split_whitespace().collect();
    if bits.len() < 2 {
        return Err(HashlineError::Parse("MV needs range and dest".into()));
    }
    let (start, end) = parse_span(bits[0], mode)?;
    let dest = parse_num(bits[1], mode)?;
    Ok((start, end, dest))
}

fn parse_put_header(
    rest: &str,
    mode: ParseMode,
) -> Result<(usize, usize, Option<String>), HashlineError> {
    let rest = match mode {
        ParseMode::Strict => rest,
        ParseMode::Sloppy => rest.trim(),
    };
    if rest.is_empty() {
        return Err(HashlineError::Parse("PUT needs a range".into()));
    }
    match mode {
        ParseMode::Strict => {
            let Some((span, after)) = rest.split_once(':') else {
                return Err(HashlineError::Parse("PUT requires ':'".into()));
            };
            let (start, end) = parse_span(span.trim(), mode)?;
            let inline = after.strip_prefix(' ').unwrap_or(after);
            if inline.is_empty() {
                Ok((start, end, None))
            } else {
                Ok((start, end, Some(inline.to_string())))
            }
        }
        ParseMode::Sloppy => {
            let rest = rest.strip_prefix(':').unwrap_or(rest).trim();
            let mut chars = rest.chars().peekable();
            let mut span = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() || c == '-' {
                    span.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            if span.is_empty() {
                return Err(HashlineError::Parse("PUT needs a range".into()));
            }
            let (start, end) = parse_span(&span, mode)?;
            let leftover: String = chars.collect();
            let leftover = leftover.trim_start_matches(':').trim();
            if leftover.is_empty() {
                Ok((start, end, None))
            } else {
                Ok((start, end, Some(strip_quotes(leftover, mode))))
            }
        }
    }
}

fn strip_quotes(s: &str, mode: ParseMode) -> String {
    if mode != ParseMode::Sloppy {
        return s.to_string();
    }
    let t = s.trim();
    if (t.starts_with('"') && t.ends_with('"') && t.len() >= 2)
        || (t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2)
    {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src() -> &'static str {
        "alpha\nbeta\ngamma\ndelta\n"
    }

    #[test]
    fn apply_put_replaces_line() {
        let c = src();
        let tag = tag_for(c);
        let out = apply(
            c,
            &tag,
            "PUT 2: BETA\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap();
        assert_eq!(out, "alpha\nBETA\ngamma\ndelta\n");
    }

    #[test]
    fn stale_tag_fails_closed() {
        let c = src();
        let err = apply(
            c,
            "deadbeefdeadbeef",
            "PUT 1: x\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, HashlineError::StaleTag { .. }));
    }

    #[test]
    fn noop_fails_closed() {
        let c = src();
        let tag = tag_for(c);
        let err = apply(
            c,
            &tag,
            "PUT 1: alpha\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert_eq!(err, HashlineError::Noop);
    }

    #[test]
    fn rem_only_is_noop() {
        let c = src();
        let tag = tag_for(c);
        let err = apply(
            c,
            &tag,
            "REM note\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert_eq!(err, HashlineError::Noop);
    }

    #[test]
    fn unseen_line_fails_closed() {
        let c = src();
        let tag = tag_for(c);
        let err = apply(
            c,
            &tag,
            "CUT 99\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, HashlineError::UnseenLine { line: 99 }));
    }

    #[test]
    fn elided_line_fails_closed() {
        let long = (1..=20)
            .map(|i| format!("L{i}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let read = format_read(
            "big.txt",
            &long,
            ReadOptions {
                max_visible: Some(6),
                head: 2,
                tail: 2,
            },
        );
        assert!(read.text.contains("[elided 3-18]"));
        assert!(!read.visible.allows(10));
        let err = apply(
            &long,
            &read.tag,
            "PUT 10: nope\n",
            &read.visible,
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, HashlineError::ElidedLine { line: 10 }));
    }

    #[test]
    fn sloppy_parse_fallback() {
        let c = src();
        let tag = tag_for(c);
        let out = apply(
            c,
            &tag,
            "put 2 beta-new\n",
            &VisibleSet::all_lines(),
            ModelFamily::Sloppy,
        )
        .unwrap();
        assert_eq!(out, "alpha\nbeta-new\ngamma\ndelta\n");
    }

    #[test]
    fn sloppy_still_rejects_ambiguous() {
        let c = src();
        let tag = tag_for(c);
        let err = apply(
            c,
            &tag,
            "put\n",
            &VisibleSet::all_lines(),
            ModelFamily::Sloppy,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            HashlineError::Ambiguous(_) | HashlineError::Parse(_)
        ));
    }

    #[test]
    fn strict_rejects_lowercase() {
        let c = src();
        let tag = tag_for(c);
        let err = apply(
            c,
            &tag,
            "put 2: x\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, HashlineError::Parse(_)));
    }

    #[test]
    fn cut_and_mv() {
        let c = src();
        let tag = tag_for(c);
        let out = apply(
            c,
            &tag,
            "CUT 2\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap();
        assert_eq!(out, "alpha\ngamma\ndelta\n");
        let tag2 = tag_for(&out);
        let moved = apply(
            &out,
            &tag2,
            "MV 3 1\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap();
        assert_eq!(moved, "delta\nalpha\ngamma\n");
    }

    #[test]
    fn format_read_header() {
        let c = src();
        let r = format_read(
            "demo.rs",
            c,
            ReadOptions {
                max_visible: None,
                head: 0,
                tail: 0,
            },
        );
        assert!(r.text.starts_with(&format!("[demo.rs#{}]", r.tag)));
        assert!(r.text.contains("1:alpha"));
        assert_eq!(r.tag, tag_for(c));
    }
}
