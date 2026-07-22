//! Repository map: structural summary of a codebase ranked by importance.
//!
//! Builds a directed graph of file-to-file references (file A references a
//! symbol defined in file B) and runs PageRank to rank files by importance.
//! The top files plus their most-referenced symbols are rendered into a
//! token-budgeted summary suitable for feeding to an LLM context window.

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors produced by [`RepoMap`].
#[derive(Debug, Error)]
pub enum RepoMapError {
    #[error("io error: {0}")]
    Io(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("cache error: {0}")]
    Cache(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedRanking {
    file_count: usize,
    ranking: Vec<(String, f64)>,
}

/// A symbol definition extracted from a source file.
#[derive(Debug, Clone)]
struct SymbolDef {
    name: String,
    kind: String,
}

/// A repository map ranked by PageRank over the file-reference graph.
pub struct RepoMap {
    workspace: PathBuf,
    cache: Option<Vec<(PathBuf, f64)>>,
}

impl RepoMap {
    /// Create a new [`RepoMap`] rooted at `workspace`.
    pub fn new(workspace: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
            cache: None,
        }
    }

    /// Build a token-budgeted summary of the most important files.
    pub fn build(&self, max_tokens: usize) -> String {
        let ranking = self.ranked();
        let symbol_index = self.build_symbol_index();
        let ref_counts = self.symbol_reference_counts(&symbol_index);

        let mut out = String::new();
        let mut tokens = 0usize;
        const CHARS_PER_TOKEN: usize = 3;

        for (path, _score) in &ranking {
            let rel = self.relative_path(path);
            let mut block = format!("{}\n", rel.display());
            if let Some(symbols) = symbol_index.get(path) {
                let mut sorted = symbols.clone();
                sorted.sort_by_key(|s| {
                    std::cmp::Reverse(ref_counts.get(&s.name).copied().unwrap_or(0))
                });
                for sym in sorted {
                    block.push_str(&format!("  {} {}\n", sym.kind, sym.name));
                }
            }
            let block_tokens = block.len() / CHARS_PER_TOKEN + 1;
            if tokens + block_tokens > max_tokens && !out.is_empty() {
                break;
            }
            out.push_str(&block);
            tokens += block_tokens;
        }
        out
    }

    /// Return files ranked by PageRank score (descending).
    pub fn rank_files(&self) -> Vec<(PathBuf, f64)> {
        self.ranked()
    }

    /// Return the importance score for a single file.
    pub fn file_importance(&self, path: &Path) -> f64 {
        let ranking = self.ranked();
        let target = self.canonicalize(path);
        for (p, score) in &ranking {
            if self.canonicalize(p) == target {
                return *score;
            }
        }
        0.0
    }

    /// Clear the in-memory cache, forcing a recomputation on next access.
    pub fn invalidate(&mut self) {
        self.cache = None;
    }

    fn ranked(&self) -> Vec<(PathBuf, f64)> {
        if let Some(cached) = &self.cache {
            return cached.clone();
        }
        if let Some(cached) = self.load_cache() {
            return cached;
        }
        let files = self.collect_source_files();
        let graph = self.build_graph(&files);
        let ranking = pagerank(&graph);
        let result: Vec<(PathBuf, f64)> = files
            .iter()
            .zip(ranking.iter())
            .map(|(p, s)| (p.clone(), *s))
            .collect();
        let _ = self.save_cache(&result);
        result
    }

    fn collect_source_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let ws_canonical = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());
        let mut stack = vec![self.workspace.clone()];
        while let Some(dir) = stack.pop() {
            // Canonicalize to resolve symlinks; reject if outside workspace or cycle.
            let canonical = match fs::canonicalize(&dir) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if !visited.insert(canonical.clone()) {
                continue;
            }
            if !canonical.starts_with(&ws_canonical) {
                continue;
            }
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    if name == "target" || name == ".git" || name == "node_modules" {
                        continue;
                    }
                    stack.push(path);
                } else if is_source_file(&path) {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }

    fn build_graph(&self, files: &[PathBuf]) -> Vec<Vec<usize>> {
        let index: HashMap<PathBuf, usize> = files
            .iter()
            .enumerate()
            .map(|(i, p)| (p.clone(), i))
            .collect();

        let mut defs: HashMap<PathBuf, Vec<SymbolDef>> = HashMap::new();
        let mut symbol_to_files: HashMap<String, HashSet<usize>> = HashMap::new();
        let mut contents: HashMap<PathBuf, String> = HashMap::new();

        for path in files {
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let symbols = extract_symbols(&content, path);
            for sym in &symbols {
                symbol_to_files
                    .entry(sym.name.clone())
                    .or_default()
                    .insert(index[path]);
            }
            contents.insert(path.clone(), content);
            defs.insert(path.clone(), symbols);
        }
        let _ = defs;

        let n = files.len();
        let mut adj: Vec<HashSet<usize>> = vec![HashSet::new(); n];
        for (i, path) in files.iter().enumerate() {
            let content = match contents.get(path) {
                Some(c) => c,
                None => continue,
            };
            for (name, targets) in &symbol_to_files {
                if targets.len() == 1 && targets.contains(&i) {
                    continue;
                }
                if contains_word(content, name) {
                    for &t in targets {
                        if t != i {
                            adj[i].insert(t);
                        }
                    }
                }
            }
        }
        adj.into_iter().map(|s| s.into_iter().collect()).collect()
    }

    fn build_symbol_index(&self) -> HashMap<PathBuf, Vec<SymbolDef>> {
        let files = self.collect_source_files();
        let mut index = HashMap::new();
        for path in &files {
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            index.insert(path.clone(), extract_symbols(&content, path));
        }
        index
    }

    fn symbol_reference_counts(
        &self,
        index: &HashMap<PathBuf, Vec<SymbolDef>>,
    ) -> HashMap<String, usize> {
        let mut name_to_files: HashMap<String, HashSet<PathBuf>> = HashMap::new();
        for (path, symbols) in index {
            for sym in symbols {
                name_to_files
                    .entry(sym.name.clone())
                    .or_default()
                    .insert(path.clone());
            }
        }
        let mut counts = HashMap::new();
        for (path, symbols) in index {
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for sym in symbols {
                if name_to_files.get(&sym.name).map(|s| s.len()).unwrap_or(0) <= 1 {
                    continue;
                }
                if contains_word(&content, &sym.name) {
                    *counts.entry(sym.name.clone()).or_insert(0) += 1;
                }
            }
        }
        counts
    }

    fn relative_path(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.workspace)
            .unwrap_or(path)
            .to_path_buf()
    }

    fn canonicalize(&self, path: &Path) -> PathBuf {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }

    fn project_hash(&self) -> String {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.workspace.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    fn cache_path(&self) -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join(".rx4")
                .join("repomap-cache")
                .join(format!("{}.json", self.project_hash())),
        )
    }

    fn load_cache(&self) -> Option<Vec<(PathBuf, f64)>> {
        let path = self.cache_path()?;
        let data = fs::read_to_string(&path).ok()?;
        let cached: CachedRanking = serde_json::from_str(&data).ok()?;
        let current = self.collect_source_files().len();
        if cached.file_count != current {
            return None;
        }
        Some(
            cached
                .ranking
                .into_iter()
                .map(|(p, s)| (PathBuf::from(p), s))
                .collect(),
        )
    }

    fn save_cache(&self, ranking: &[(PathBuf, f64)]) -> Result<(), RepoMapError> {
        let path = match self.cache_path() {
            Some(p) => p,
            None => return Ok(()),
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| RepoMapError::Cache(e.to_string()))?;
        }
        let cached = CachedRanking {
            file_count: self.collect_source_files().len(),
            ranking: ranking
                .iter()
                .map(|(p, s)| (p.to_string_lossy().into_owned(), *s))
                .collect(),
        };
        let data =
            serde_json::to_string(&cached).map_err(|e| RepoMapError::Cache(e.to_string()))?;
        fs::write(&path, data).map_err(|e| RepoMapError::Cache(e.to_string()))?;
        Ok(())
    }
}

fn is_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go")
    )
}

fn extract_symbols(content: &str, path: &Path) -> Vec<SymbolDef> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "rs" => extract_rust(content),
        "ts" | "tsx" | "js" | "jsx" => extract_js(content),
        "py" => extract_python(content),
        "go" => extract_go(content),
        _ => extract_generic(content),
    }
}

fn first_token_after(line: &str, keyword: &str) -> Option<String> {
    let rest = line.trim_start();
    let stripped = rest.strip_prefix(keyword)?;
    let mut iter = stripped
        .trim_start()
        .split(|c: char| !c.is_alphanumeric() && c != '_');
    let name = iter.next()?.to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn strip_rust_modifiers(line: &str) -> &str {
    let mut rest = line.trim_start();
    loop {
        let stripped = rest
            .strip_prefix("pub(crate) ")
            .or_else(|| rest.strip_prefix("pub(super) "))
            .or_else(|| rest.strip_prefix("pub "))
            .or_else(|| rest.strip_prefix("async "))
            .or_else(|| rest.strip_prefix("unsafe "))
            .or_else(|| rest.strip_prefix("extern "))
            .or_else(|| rest.strip_prefix("crate "));
        match stripped {
            Some(s) => rest = s,
            None => break,
        }
    }
    rest
}

fn extract_rust(content: &str) -> Vec<SymbolDef> {
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = strip_rust_modifiers(line);
        for keyword in ["fn ", "struct ", "enum ", "trait ", "mod "] {
            if trimmed.starts_with(keyword) {
                if let Some(name) = first_token_after(trimmed, keyword) {
                    out.push(SymbolDef {
                        name,
                        kind: keyword.trim().to_string(),
                    });
                }
                break;
            }
        }
        if trimmed.starts_with("impl ") {
            let rest = trimmed.strip_prefix("impl ").unwrap_or("");
            let name = rest
                .trim_start()
                .split(['<', ' ', '{'])
                .next()
                .unwrap_or("")
                .trim_end_matches('>');
            if !name.is_empty() {
                out.push(SymbolDef {
                    name: name.to_string(),
                    kind: "impl".to_string(),
                });
            }
        }
    }
    out
}

fn extract_js(content: &str) -> Vec<SymbolDef> {
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        for keyword in ["function ", "class ", "interface "] {
            if trimmed.starts_with(keyword) {
                if let Some(name) = first_token_after(trimmed, keyword) {
                    out.push(SymbolDef {
                        name,
                        kind: keyword.trim().to_string(),
                    });
                }
                break;
            }
        }
        if let Some(rest) = trimmed.strip_prefix("export ") {
            for keyword in ["function ", "class ", "interface "] {
                if let Some(name) = first_token_after(rest, keyword) {
                    out.push(SymbolDef {
                        name,
                        kind: keyword.trim().to_string(),
                    });
                    break;
                }
            }
        }
    }
    out
}

fn extract_python(content: &str) -> Vec<SymbolDef> {
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        for keyword in ["def ", "class "] {
            if trimmed.starts_with(keyword) {
                if let Some(name) = first_token_after(trimmed, keyword) {
                    out.push(SymbolDef {
                        name,
                        kind: keyword.trim().to_string(),
                    });
                }
                break;
            }
        }
    }
    out
}

fn extract_go(content: &str) -> Vec<SymbolDef> {
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("func ") {
            let rest = trimmed.strip_prefix("func ").unwrap_or("");
            let rest = rest.strip_prefix('(').unwrap_or(rest);
            if let Some(name) = first_token_after(rest, "") {
                if !name.is_empty() {
                    out.push(SymbolDef {
                        name,
                        kind: "func".to_string(),
                    });
                }
            }
        }
        if trimmed.starts_with("type ") {
            if let Some(name) = first_token_after(trimmed, "type ") {
                out.push(SymbolDef {
                    name,
                    kind: "type".to_string(),
                });
            }
        }
    }
    out
}

fn extract_generic(content: &str) -> Vec<SymbolDef> {
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if let Some(idx) = trimmed.find(['(', '{']) {
            let head = trimmed[..idx].trim_end_matches(':');
            if let Some(name) = head.split_whitespace().last() {
                if name
                    .chars()
                    .next()
                    .map(|c| c.is_uppercase() || c == '_')
                    .unwrap_or(false)
                    && !name.is_empty()
                {
                    out.push(SymbolDef {
                        name: name.to_string(),
                        kind: "def".to_string(),
                    });
                }
            }
        }
    }
    out
}

fn contains_word(content: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    #[cfg(feature = "builtin-tools")]
    {
        if let Ok(re) = regex::Regex::new(&format!(r"\b{}\b", regex::escape(word))) {
            return re.is_match(content);
        }
    }
    contains_word_naive(content, word)
}

fn contains_word_naive(content: &str, word: &str) -> bool {
    let bytes = content.as_bytes();
    let w = word.as_bytes();
    if w.is_empty() || bytes.len() < w.len() {
        return false;
    }
    let is_boundary = |b: u8| !((b as char).is_alphanumeric() || b == b'_');
    let mut i = 0;
    while i + w.len() <= bytes.len() {
        if &bytes[i..i + w.len()] == w {
            let left_ok = i == 0 || is_boundary(bytes[i - 1]);
            let right_ok = i + w.len() == bytes.len() || is_boundary(bytes[i + w.len()]);
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn pagerank(adj: &[Vec<usize>]) -> Vec<f64> {
    let n = adj.len();
    if n == 0 {
        return Vec::new();
    }
    let d = 0.85;
    let iterations = 20;
    let mut score = vec![1.0 / n as f64; n];
    let out_degree: Vec<usize> = adj.iter().map(|l| l.len()).collect();

    for _ in 0..iterations {
        let mut next = vec![(1.0 - d) / n as f64; n];
        let mut dangling = 0.0;
        for (i, deg) in out_degree.iter().enumerate() {
            if *deg == 0 {
                dangling += score[i];
            }
        }
        let dangling_share = d * dangling / n as f64;
        for (i, links) in adj.iter().enumerate() {
            let share = d * score[i] / out_degree[i] as f64;
            for &j in links {
                next[j] += share;
            }
        }
        for s in next.iter_mut() {
            *s += dangling_share;
        }
        score = next;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, rel: &str, content: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn extract_rust_symbols() {
        let content = "\
pub fn main() {}
struct Config {}
enum State {}
trait Provider {}
impl Provider for Config {}
mod inner {}
";
        let syms = extract_rust(content);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"State"));
        assert!(names.contains(&"Provider"));
        assert!(names.contains(&"inner"));
    }

    #[test]
    fn extract_typescript_symbols() {
        let content = "\
function greet() {}
class Greeter {}
interface IGreeter {}
export function farewell() {}
export class Farewell {}
";
        let syms = extract_js(content);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"greet"));
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"IGreeter"));
        assert!(names.contains(&"farewell"));
        assert!(names.contains(&"Farewell"));
    }

    #[test]
    fn extract_python_symbols() {
        let content = "\
def hello():
    pass

class World:
    pass
";
        let syms = extract_python(content);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"hello"));
        assert!(names.contains(&"World"));
    }

    #[test]
    fn extract_go_symbols() {
        let content = "\
func main() {}

type Server struct {}
";
        let syms = extract_go(content);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"Server"));
    }

    #[test]
    fn pagerank_converges() {
        let adj = vec![vec![1], vec![2], vec![0]];
        let scores = pagerank(&adj);
        assert_eq!(scores.len(), 3);
        let total: f64 = scores.iter().sum();
        assert!((total - 1.0).abs() < 1e-9);
        assert!(scores.iter().all(|s| *s > 0.0));
    }

    #[test]
    fn pagerank_dangling_node() {
        let adj = vec![vec![1], vec![]];
        let scores = pagerank(&adj);
        let total: f64 = scores.iter().sum();
        assert!((total - 1.0).abs() < 1e-9);
    }

    #[test]
    fn token_budget_stops_at_limit() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "a.rs",
            "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n",
        );
        write(dir.path(), "b.rs", "fn delta() {}\n");
        let map = RepoMap::new(dir.path());
        let summary = map.build(5);
        let tokens = summary.len() / 3 + 1;
        assert!(tokens <= 20, "summary tokens {tokens} should be near limit");
        assert!(!summary.is_empty());
    }

    #[test]
    fn more_referenced_file_ranks_higher() {
        let dir = tempdir().unwrap();
        write(dir.path(), "core.rs", "pub struct Core {}\n");
        write(
            dir.path(),
            "a.rs",
            "use crate::core::Core;\nfn a() -> Core { Core {} }\n",
        );
        write(
            dir.path(),
            "b.rs",
            "use crate::core::Core;\nfn b() -> Core { Core {} }\n",
        );
        write(dir.path(), "c.rs", "fn c() {}\n");
        let map = RepoMap::new(dir.path());
        let ranking = map.rank_files();
        let score = |name: &str| -> f64 {
            ranking
                .iter()
                .find(|(p, _)| p.file_name().unwrap().to_string_lossy() == name)
                .map(|(_, s)| *s)
                .unwrap_or(0.0)
        };
        let core = score("core.rs");
        let c = score("c.rs");
        assert!(core > c, "core.rs ({core}) should outrank c.rs ({c})");
    }

    #[test]
    fn file_importance_returns_score() {
        let dir = tempdir().unwrap();
        let path = write(dir.path(), "a.rs", "fn alpha() {}\n");
        let map = RepoMap::new(dir.path());
        let score = map.file_importance(&path);
        assert!(score > 0.0);
    }

    #[test]
    fn cache_save_and_load() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.rs", "fn alpha() {}\n");
        write(dir.path(), "b.rs", "fn beta() {}\n");
        let map = RepoMap::new(dir.path());
        let first = map.rank_files();
        let loaded = map.load_cache();
        assert!(loaded.is_some(), "cache should be saved to disk");
        let loaded = loaded.unwrap();
        assert_eq!(loaded.len(), first.len());
        for ((p1, s1), (p2, s2)) in first.iter().zip(loaded.iter()) {
            assert_eq!(p1.file_name(), p2.file_name());
            assert!((s1 - s2).abs() < 1e-9);
        }
    }

    #[test]
    fn cache_invalidates_on_file_count_change() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.rs", "fn alpha() {}\n");
        let map = RepoMap::new(dir.path());
        let _ = map.rank_files();
        write(dir.path(), "b.rs", "fn beta() {}\n");
        let loaded = map.load_cache();
        assert!(loaded.is_none(), "stale cache should be ignored");
    }

    #[test]
    fn invalidate_clears_cache() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.rs", "fn alpha() {}\n");
        let mut map = RepoMap::new(dir.path());
        let _ = map.rank_files();
        map.invalidate();
        assert!(map.cache.is_none());
    }

    #[test]
    fn contains_word_matches_identifiers() {
        assert!(contains_word_naive("foo bar baz", "bar"));
        assert!(!contains_word_naive("foobar baz", "bar"));
        assert!(contains_word("fn main() {}", "main"));
    }
}
