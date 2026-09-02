use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct SymbolDef {
    name: String,
    kind: String,
}

fn symbol_reference_counts_original(
    index: &HashMap<String, Vec<SymbolDef>>,
) -> HashMap<String, usize> {
    let mut name_to_files: HashMap<String, HashSet<String>> = HashMap::new();
    for (path, symbols) in index {
        for sym in symbols {
            name_to_files
                .entry(sym.name.clone())
                .or_default()
                .insert(path.clone());
        }
    }
    let mut counts = HashMap::new();
    for symbols in index.values() {
        // Mock get_file_words
        let words: HashSet<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        for sym in symbols {
            if name_to_files.get(&sym.name).map(|s| s.len()).unwrap_or(0) <= 1 {
                continue;
            }
            if words.contains(sym.name.as_str()) {
                *counts.entry(sym.name.clone()).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn symbol_reference_counts_optimized(
    index: &HashMap<String, Vec<SymbolDef>>,
) -> HashMap<String, usize> {
    let mut name_to_files: HashMap<String, HashSet<String>> = HashMap::new();
    for (path, symbols) in index {
        for sym in symbols {
            name_to_files
                .entry(sym.name.clone())
                .or_default()
                .insert(path.clone());
        }
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for symbols in index.values() {
        let words: HashSet<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        for sym in symbols {
            if name_to_files.get(&sym.name).map(|s| s.len()).unwrap_or(0) <= 1 {
                continue;
            }
            if words.contains(sym.name.as_str()) {
                if let Some(c) = counts.get_mut(&sym.name) {
                    *c += 1;
                } else {
                    counts.insert(sym.name.clone(), 1);
                }
            }
        }
    }
    counts
}

fn bench_repomap(c: &mut Criterion) {
    let mut index = HashMap::new();
    for i in 0..100 {
        let mut symbols = Vec::new();
        for j in 0..1000 {
            symbols.push(SymbolDef {
                name: format!("symbol_{}", j % 100),
                kind: "def".to_string(),
            });
        }
        index.insert(format!("path_{i}"), symbols);
    }

    c.bench_function("repomap_original", |b| {
        b.iter(|| symbol_reference_counts_original(black_box(&index)))
    });
    c.bench_function("repomap_optimized", |b| {
        b.iter(|| symbol_reference_counts_optimized(black_box(&index)))
    });
}

criterion_group!(benches, bench_repomap);
criterion_main!(benches);
