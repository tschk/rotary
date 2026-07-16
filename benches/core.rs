//! Core benchmarks: session I/O, compaction, SSE parsing, tool registry,
//! secret redaction, sandbox validation, and memory FTS5 search.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use rx4::compaction::{compact_messages, CompactionConfig};
use rx4::pi::PiSession;
use rx4::provider::{Message, Role};
use rx4::secrets::Redactor;
use rx4::sse::SseParser;

fn bench_session_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("session/append");
    for n in [100, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                let mut s = PiSession::new("/bench", "gpt-4o");
                for i in 0..n {
                    s.append_message(Role::User, format!("message {i}"));
                }
                black_box(&s);
            });
        });
    }
    group.finish();
}

fn bench_session_jsonl_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("session/jsonl_roundtrip");
    for n in [100, 1_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let tmp = tempfile::tempdir().unwrap();
            let mut s = PiSession::new("/bench", "gpt-4o");
            for i in 0..n {
                s.append_message(Role::User, format!("message {i}"));
            }
            b.iter(|| {
                let path = s.save_jsonl(tmp.path()).unwrap();
                let loaded = PiSession::load_jsonl(&path).unwrap();
                black_box(loaded);
            });
        });
    }
    group.finish();
}

fn bench_compaction(c: &mut Criterion) {
    let mut group = c.benchmark_group("compaction");
    for n in [100, 500, 1_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let mut messages = Vec::with_capacity(n);
            messages.push(Message::system("system prompt"));
            for i in 0..n {
                messages.push(Message::user(format!(
                    "This is message number {i} with some content to fill tokens. "
                )));
            }
            let config = CompactionConfig::new(500, 100, 50);
            b.iter(|| {
                let result = compact_messages(black_box(&messages), black_box(&config));
                black_box(result);
            });
        });
    }
    group.finish();
}

fn bench_sse_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("sse/parse");
    for n in [10, 100, 1_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let mut chunk = String::new();
            for i in 0..n {
                chunk.push_str(&format!("data: event payload {i}\n\n"));
            }
            let bytes = chunk.as_bytes();
            b.iter(|| {
                let mut parser = SseParser::new();
                let events = parser.feed(black_box(bytes));
                black_box(events);
            });
        });
    }
    group.finish();
}

fn bench_secret_redaction(c: &mut Criterion) {
    let redactor = Redactor::new();
    let mut group = c.benchmark_group("secrets/redact");
    for n in [1, 10, 100] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let mut text = String::new();
            for i in 0..n {
                text.push_str(&format!(
                    "API key: sk-abc123def456ghi789jkl012mno345pqr678stu901vwx234yza567b\nmessage {i}\n"
                ));
            }
            b.iter(|| {
                let redacted = redactor.redact(black_box(&text));
                black_box(redacted);
            });
        });
    }
    group.finish();
}

fn bench_tool_registry_lookup(c: &mut Criterion) {
    use rx4::{ToolDefinition, ToolRegistry};
    let mut registry = ToolRegistry::new();
    for i in 0..100 {
        registry.register(ToolDefinition::new_fn(
            format!("tool_{i}"),
            "bench tool",
            r#"{"type":"object"}"#,
            |_ctx, _args| Box::pin(async { rx4::ToolResult::ok("bench", "ok") }),
        ));
    }
    c.bench_function("tool_registry/lookup", |b| {
        b.iter(|| {
            let defs = registry.definitions();
            black_box(defs.len());
        });
    });
}

fn bench_token_estimation(c: &mut Criterion) {
    use rx4::compaction::estimate_tokens;
    let text = "x".repeat(10_000);
    c.bench_function("compaction/estimate_tokens_10k", |b| {
        b.iter(|| {
            let tokens = estimate_tokens(black_box(&text));
            black_box(tokens);
        });
    });
}

fn bench_message_estimation(c: &mut Criterion) {
    use rx4::compaction::estimate_messages;
    let messages: Vec<Message> = (0..100)
        .map(|i| Message::user(format!("message {i} with content")))
        .collect();
    c.bench_function("compaction/estimate_messages_100", |b| {
        b.iter(|| {
            let tokens = estimate_messages(black_box(&messages));
            black_box(tokens);
        });
    });
}

criterion_group!(
    benches,
    bench_session_append,
    bench_session_jsonl_roundtrip,
    bench_compaction,
    bench_sse_parsing,
    bench_secret_redaction,
    bench_tool_registry_lookup,
    bench_token_estimation,
    bench_message_estimation,
);
criterion_main!(benches);
