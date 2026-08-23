//! Hashline edit protocol: tagged reads and fail-closed PUT/CUT/MV/REM.
//!
//! Hosts call this API instead of owning their own line-edit dialect.
//! A read is `[path#TAG]` plus `N:line`. Edits name a tag and speak PUT, CUT,
//! MV, and REM. Stale tags, elided lines, unseen lines, and no-ops fail closed.

use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
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
    /// 0-based start line. Visible output never exceeds `max_visible`.
    pub offset: usize,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            max_visible: Some(400),
            head: 200,
            tail: 80,
            offset: 0,
        }
    }
}

impl ReadOptions {
    /// Derive `head`/`tail` from `limit` only so they never exceed `max_visible`.
    ///
    /// `offset` is applied separately via [`ReadOptions::with_offset`].
    pub fn from_limit(limit: usize) -> Self {
        let max_visible = limit.max(1);
        let tail = max_visible / 4;
        let head = max_visible - tail;
        debug_assert!(head + tail == max_visible);
        Self {
            max_visible: Some(max_visible),
            head,
            tail,
            offset: 0,
        }
    }

    /// Treat `offset` as a 0-based start line without growing `max_visible`.
    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }
}

/// Last hashline-tagged read per path. Missing or mismatched tags fail closed
/// (nothing is treated as visible).
#[derive(Debug, Clone, Default)]
pub struct HashlineSight {
    by_path: HashMap<String, SightEntry>,
}

#[derive(Debug, Clone)]
struct SightEntry {
    tag: String,
    visible: VisibleSet,
}

pub fn normalize_sight_path(path: &str) -> String {
    let p = path.trim().replace('\\', "/");
    match p.strip_prefix("./") {
        Some(rest) => rest.to_string(),
        None => p,
    }
}

impl HashlineSight {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remember(&mut self, path: &str, tag: impl Into<String>, visible: VisibleSet) {
        self.by_path.insert(
            normalize_sight_path(path),
            SightEntry {
                tag: tag.into(),
                visible,
            },
        );
    }

    pub fn forget(&mut self, path: &str) {
        self.by_path.remove(&normalize_sight_path(path));
    }

    /// Visible lines from the last hashline read of `path` whose tag matches.
    /// Empty (fail closed) when there is no prior read or the tag differs.
    pub fn visible_for(&self, path: &str, tag: &str) -> VisibleSet {
        self.visible_for_any([path], tag)
    }

    /// First matching prior read among `paths` whose tag equals `tag`.
    /// Empty (fail closed) when none match.
    pub fn visible_for_any(
        &self,
        paths: impl IntoIterator<Item = impl AsRef<str>>,
        tag: &str,
    ) -> VisibleSet {
        for path in paths {
            if let Some(e) = self
                .by_path
                .get(&normalize_sight_path(path.as_ref()))
                .filter(|e| e.tag == tag)
            {
                return e.visible.clone();
            }
        }
        VisibleSet::from_lines([])
    }
}

/// Fail-closed hashline errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashlineError {
    StaleTag {
        expected: String,
        actual: String,
    },
    UnseenLine {
        line: usize,
    },
    ElidedLine {
        line: usize,
    },
    Noop,
    Parse(String),
    Ambiguous(String),
    EmptyScript,
    /// Line number exceeds the *current* buffer (e.g. PUT after CUT).
    OutOfRange {
        line: usize,
    },
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
            Self::OutOfRange { line } => write!(f, "out of range line {line}"),
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
    let start = opts.offset.min(n);
    let remaining = n.saturating_sub(start);
    let (head, tail) = clamp_head_tail(opts.head, opts.tail, opts.max_visible);
    let (visible, show): (VisibleSet, Vec<usize>) = match opts.max_visible {
        Some(max) if remaining > max && head + tail < remaining => {
            let head = head.min(remaining);
            let tail = tail.min(remaining.saturating_sub(head));
            let head_end = start + head;
            let tail_start = n - tail + 1;
            let mut vis = BTreeSet::new();
            let mut order = Vec::new();
            for i in (start + 1)..=head_end {
                vis.insert(i);
                order.push(i);
            }
            for i in tail_start..=n {
                vis.insert(i);
                order.push(i);
            }
            (VisibleSet::from_lines(vis), order)
        }
        _ if start == 0 => (VisibleSet::all_lines(), (1..=n).collect()),
        _ => (
            VisibleSet::from_lines((start + 1)..=n),
            ((start + 1)..=n).collect(),
        ),
    };

    let mut last = 0usize;
    for i in &show {
        if *i > last + 1 {
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
    let (mut lines, ended_nl, eol) = split_keep(content);
    // Elision is against the tagged snapshot. Bounds are checked per-op
    // against the *current* buffer so CUT-then-PUT cannot panic.
    for op in &ops {
        match op {
            Op::Put { start, end, .. } | Op::Cut { start, end } => {
                check_visible(*start, *end, visible)?;
            }
            Op::Mv { start, end, dest } => {
                check_visible(*start, *end, visible)?;
                if !visible.allows(*dest) {
                    return Err(HashlineError::ElidedLine { line: *dest });
                }
            }
            Op::Rem => {}
        }
    }
    for op in ops {
        apply_op(&mut lines, op)?;
    }
    let mut next = lines.join(eol);
    if (ended_nl || content.is_empty()) && !next.ends_with('\n') {
        next.push_str(eol);
    }
    if content.is_empty() && (next == "\n" || next == "\r\n") {
        next.clear();
    }
    if next == content {
        return Err(HashlineError::Noop);
    }
    Ok(next)
}

fn clamp_head_tail(head: usize, tail: usize, max_visible: Option<usize>) -> (usize, usize) {
    let Some(max) = max_visible else {
        return (head, tail);
    };
    if head + tail <= max {
        return (head, tail);
    }
    let tail = max / 4;
    (max - tail, tail)
}

fn check_visible(start: usize, end: usize, visible: &VisibleSet) -> Result<(), HashlineError> {
    if start == 0 || end == 0 || start > end {
        return Err(HashlineError::Parse(format!("bad range {start}-{end}")));
    }
    if !visible.allows_range(start, end) {
        return Err(HashlineError::ElidedLine { line: start });
    }
    Ok(())
}

fn check_current_range(start: usize, end: usize, len: usize) -> Result<(), HashlineError> {
    if start == 0 || end == 0 || start > end {
        return Err(HashlineError::Parse(format!("bad range {start}-{end}")));
    }
    if start > len || end > len {
        return Err(HashlineError::OutOfRange {
            line: start.max(end),
        });
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
            check_current_range(start, end, lines.len())?;
            lines.splice(start - 1..end, repl);
            Ok(())
        }
        Op::Cut { start, end } => {
            check_current_range(start, end, lines.len())?;
            lines.drain(start - 1..end);
            Ok(())
        }
        Op::Mv { start, end, dest } => {
            check_current_range(start, end, lines.len())?;
            if dest == 0 || dest > lines.len() + 1 {
                return Err(HashlineError::OutOfRange { line: dest });
            }
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
                return Err(HashlineError::OutOfRange { line: dest });
            }
            for (i, line) in block.into_iter().enumerate() {
                lines.insert(insert_at + i, line);
            }
            Ok(())
        }
    }
}

fn split_keep(content: &str) -> (Vec<String>, bool, &'static str) {
    if content.is_empty() {
        return (Vec::new(), false, "\n");
    }
    let eol = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let ended = content.ends_with('\n');
    let mut lines: Vec<String> = content
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    if ended {
        lines.pop();
    }
    (lines, ended, eol)
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
    let mut parts = rest.split_whitespace();
    let token = parts.next().unwrap_or("");
    if mode == ParseMode::Strict && parts.next().is_some() {
        return Err(HashlineError::Parse(format!(
            "trailing tokens after range: {rest}"
        )));
    }
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
        assert!(matches!(err, HashlineError::OutOfRange { line: 99 }));
    }

    #[test]
    fn cut_then_put_out_of_range_does_not_panic() {
        let c = src();
        let tag = tag_for(c);
        // CUT 1-4 empties the file; PUT 3 would panic if ranges were only
        // checked against the original length.
        let err = apply(
            c,
            &tag,
            "CUT 1-4\nPUT 3: nope\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, HashlineError::OutOfRange { line: 3 }));
    }

    #[test]
    fn sequential_cut_then_put_on_remaining_line() {
        let c = src();
        let tag = tag_for(c);
        let out = apply(
            c,
            &tag,
            "CUT 2-3\nPUT 2: DELTA\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap();
        assert_eq!(out, "alpha\nDELTA\n");
    }

    #[test]
    fn from_limit_head_plus_tail_never_exceeds_max() {
        for limit in [0, 1, 2, 5, 10, 50, 400] {
            let opts = ReadOptions::from_limit(limit);
            let max = opts.max_visible.unwrap();
            assert_eq!(opts.head + opts.tail, max);
            assert!(opts.head + opts.tail <= max);
        }
    }

    #[test]
    fn format_read_clamps_head_tail_that_exceed_max() {
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
                head: 100,
                tail: 80,
                offset: 0,
            },
        );
        // 6 = head 5 + tail 1 after clamp
        assert!(read.visible.allows(1));
        assert!(read.visible.allows(5));
        assert!(!read.visible.allows(6));
        assert!(read.visible.allows(20));
        assert!(!read.visible.allows(10));
    }

    #[test]
    fn sight_fail_closed_without_prior_read() {
        let sight = HashlineSight::new();
        let vis = sight.visible_for("a.rs", "deadbeef");
        assert!(!vis.allows(1));
        let c = src();
        let tag = tag_for(c);
        let err = apply(c, &tag, "PUT 1: x\n", &vis, ModelFamily::Strict).unwrap_err();
        assert!(matches!(err, HashlineError::ElidedLine { line: 1 }));
    }

    #[test]
    fn sight_remembers_last_hashline_read() {
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
                offset: 0,
            },
        );
        let mut sight = HashlineSight::new();
        sight.remember("./big.txt", &read.tag, read.visible.clone());
        let vis = sight.visible_for("big.txt", &read.tag);
        assert!(vis.allows(1));
        assert!(!vis.allows(10));
        let err = apply(
            &long,
            &read.tag,
            "PUT 10: nope\n",
            &vis,
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, HashlineError::ElidedLine { line: 10 }));
        sight.forget("big.txt");
        assert!(!sight.visible_for("big.txt", &read.tag).allows(1));
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
                offset: 0,
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
                offset: 0,
            },
        );
        assert!(r.text.starts_with(&format!("[demo.rs#{}]", r.tag)));
        assert!(r.text.contains("1:alpha"));
        assert_eq!(r.tag, tag_for(c));
    }

    #[test]
    fn preserve_crlf_on_rewrite() {
        let c = "alpha\r\nbeta\r\ngamma\r\n";
        let tag = tag_for(c);
        let out = apply(
            c,
            &tag,
            "PUT 2: BETA\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap();
        assert_eq!(out, "alpha\r\nBETA\r\ngamma\r\n");
        // Unchanged PUT must still be a no-op (CRLF not rewritten to LF).
        let err = apply(
            c,
            &tag,
            "PUT 2: beta\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert_eq!(err, HashlineError::Noop);
    }

    #[test]
    fn sequential_cut_then_cut_out_of_range() {
        let c = src();
        let tag = tag_for(c);
        let err = apply(
            c,
            &tag,
            "CUT 1\nCUT 4\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, HashlineError::OutOfRange { line: 4 }));
    }

    #[test]
    fn strict_cut_rejects_trailing_tokens() {
        let c = src();
        let tag = tag_for(c);
        let err = apply(
            c,
            &tag,
            "CUT 2 garbage\n",
            &VisibleSet::all_lines(),
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, HashlineError::Parse(_)));
    }

    #[test]
    fn mv_dest_must_be_visible() {
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
                offset: 0,
            },
        );
        let err = apply(
            &long,
            &read.tag,
            "MV 1 10\n",
            &read.visible,
            ModelFamily::Strict,
        )
        .unwrap_err();
        assert!(matches!(err, HashlineError::ElidedLine { line: 10 }));
    }

    #[test]
    fn format_read_honors_offset_within_max_visible() {
        let long = (1..=50)
            .map(|i| format!("L{i}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let read = format_read(
            "big.txt",
            &long,
            ReadOptions::from_limit(10).with_offset(20),
        );
        let vis = &read.visible;
        assert!(!vis.allows(1));
        assert!(vis.allows(21));
        assert!(read.text.contains("21:L21"));
        assert!(!read.text.contains("1:L1\n") || !vis.allows(1));
        let shown = (1..=50).filter(|i| vis.allows(*i)).count();
        assert!(shown <= 10);
    }
}
