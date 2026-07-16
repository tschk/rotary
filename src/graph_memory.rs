//! Graph memory: a knowledge graph for codebase structure and agent memory.
//!
//! Inspired by graphify-rs concepts, this module provides an in-memory
//! knowledge graph that records codebase structure (files, functions, classes,
//! imports) and agent memory (concepts, decisions, patterns, bugs). It
//! supports PageRank, shortest path, community detection via label
//! propagation, JSON persistence, conversation extraction, codebase scanning,
//! and nightly dream consolidation.
//!
//! Graph operations use a simple adjacency list (`HashMap<String, Vec<String>>`)
//! rather than an external graph crate, keeping the dependency tree light.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

static NODE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_node_id() -> String {
    let n = NODE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("n{}", n)
}

/// Errors returned by graph memory operations.
#[derive(Debug, Error)]
pub enum GraphMemoryError {
    #[error("io error: {0}")]
    Io(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("graph error: {0}")]
    GraphError(String),
}

impl From<std::io::Error> for GraphMemoryError {
    fn from(err: std::io::Error) -> Self {
        GraphMemoryError::Io(err.to_string())
    }
}

impl From<serde_json::Error> for GraphMemoryError {
    fn from(err: serde_json::Error) -> Self {
        GraphMemoryError::Serialization(err.to_string())
    }
}

/// The kind of entity a [`MemoryNode`] represents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeType {
    Function,
    Class,
    Module,
    File,
    Concept,
    Decision,
    Pattern,
    Bug,
    Feature,
    Other,
}

impl NodeType {
    fn as_str(&self) -> &'static str {
        match self {
            NodeType::Function => "Function",
            NodeType::Class => "Class",
            NodeType::Module => "Module",
            NodeType::File => "File",
            NodeType::Concept => "Concept",
            NodeType::Decision => "Decision",
            NodeType::Pattern => "Pattern",
            NodeType::Bug => "Bug",
            NodeType::Feature => "Feature",
            NodeType::Other => "Other",
        }
    }
}

/// The kind of relationship a [`MemoryEdge`] represents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeRelation {
    Calls,
    Imports,
    Implements,
    References,
    Depends,
    Contains,
    RelatedTo,
    FixedBy,
    CreatedBy,
    ModifiedBy,
}

/// A node in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNode {
    pub id: String,
    pub label: String,
    pub node_type: NodeType,
    pub description: String,
    pub source_file: Option<String>,
    pub source_location: Option<String>,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
}

/// A directed edge in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEdge {
    pub source: String,
    pub target: String,
    pub relation: EdgeRelation,
    pub confidence: f64,
    pub source_file: Option<String>,
}

/// Aggregate statistics about a [`GraphMemory`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub density: f64,
    pub avg_degree: f64,
    pub type_distribution: HashMap<String, usize>,
}

/// A knowledge graph for codebase structure and agent memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphMemory {
    nodes: HashMap<String, MemoryNode>,
    edges: Vec<MemoryEdge>,
    adjacency: HashMap<String, Vec<String>>,
    reverse: HashMap<String, Vec<String>>,
    workspace: Option<PathBuf>,
}

impl Default for GraphMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphMemory {
    /// Create an empty graph memory.
    pub fn new() -> Self {
        GraphMemory {
            nodes: HashMap::new(),
            edges: Vec::new(),
            adjacency: HashMap::new(),
            reverse: HashMap::new(),
            workspace: None,
        }
    }

    /// Create a graph memory bound to a workspace path.
    pub fn from_workspace(workspace: &Path) -> Self {
        let mut g = Self::new();
        g.workspace = Some(workspace.to_path_buf());
        g
    }

    /// Add a node to the graph, returning the node id.
    pub fn add_node(&mut self, mut node: MemoryNode) -> String {
        if node.id.is_empty() {
            node.id = next_node_id();
        }
        let id = node.id.clone();
        self.adjacency.entry(id.clone()).or_default();
        self.reverse.entry(id.clone()).or_default();
        self.nodes.insert(id.clone(), node);
        id
    }

    /// Add a directed edge. Returns an error if either endpoint is missing.
    pub fn add_edge(&mut self, edge: MemoryEdge) -> Result<(), GraphMemoryError> {
        if !self.nodes.contains_key(&edge.source) {
            return Err(GraphMemoryError::NotFound(format!(
                "source node: {}",
                edge.source
            )));
        }
        if !self.nodes.contains_key(&edge.target) {
            return Err(GraphMemoryError::NotFound(format!(
                "target node: {}",
                edge.target
            )));
        }
        self.adjacency
            .entry(edge.source.clone())
            .or_default()
            .push(edge.target.clone());
        self.reverse
            .entry(edge.target.clone())
            .or_default()
            .push(edge.source.clone());
        self.edges.push(edge);
        Ok(())
    }

    /// Look up a node by id.
    pub fn get_node(&self, id: &str) -> Option<&MemoryNode> {
        self.nodes.get(id)
    }

    /// Return the depth-1 neighbors (successors) of a node.
    pub fn get_neighbors(&self, id: &str) -> Vec<&MemoryNode> {
        self.adjacency
            .get(id)
            .map(|v| v.iter().filter_map(|n| self.nodes.get(n)).collect())
            .unwrap_or_default()
    }

    /// Keyword search across node labels and descriptions (case-insensitive).
    pub fn search(&self, query: &str) -> Vec<&MemoryNode> {
        let q = query.to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().collect();
        self.nodes
            .values()
            .filter(|n| {
                let label = n.label.to_lowercase();
                let desc = n.description.to_lowercase();
                let tags: Vec<String> = n.tags.iter().map(|t| t.to_lowercase()).collect();
                terms.iter().all(|t| {
                    label.contains(t) || desc.contains(t) || tags.iter().any(|tag| tag.contains(t))
                })
            })
            .collect()
    }

    /// Compute PageRank scores (damping 0.85, 20 iterations) for all nodes.
    pub fn pagerank(&self) -> Vec<(String, f64)> {
        let n = self.nodes.len();
        if n == 0 {
            return Vec::new();
        }
        let d = 0.85_f64;
        let init = 1.0 / n as f64;
        let mut scores: HashMap<String, f64> =
            self.nodes.keys().map(|k| (k.clone(), init)).collect();
        let out_degree: HashMap<String, usize> = self
            .nodes
            .keys()
            .map(|k| {
                (
                    k.clone(),
                    self.adjacency.get(k).map(|v| v.len()).unwrap_or(0),
                )
            })
            .collect();

        for _ in 0..20 {
            let mut next: HashMap<String, f64> = HashMap::with_capacity(n);
            let dangling_sum: f64 = self
                .nodes
                .keys()
                .filter(|k| out_degree.get(*k).copied().unwrap_or(0) == 0)
                .map(|k| scores[k])
                .sum();
            for k in self.nodes.keys() {
                let mut rank = (1.0 - d) / n as f64;
                rank += d * dangling_sum / n as f64;
                if let Some(preds) = self.reverse.get(k) {
                    for p in preds {
                        let deg = out_degree.get(p).copied().unwrap_or(0);
                        if deg > 0 {
                            rank += d * scores[p] / deg as f64;
                        }
                    }
                }
                next.insert(k.clone(), rank);
            }
            scores = next;
        }
        let mut v: Vec<(String, f64)> = scores.into_iter().collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v
    }

    /// Compute the BFS shortest path (by node ids) between two nodes.
    pub fn shortest_path(&self, from: &str, to: &str) -> Vec<String> {
        if !self.nodes.contains_key(from) || !self.nodes.contains_key(to) {
            return Vec::new();
        }
        if from == to {
            return vec![from.to_string()];
        }
        let mut prev: HashMap<String, String> = HashMap::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        queue.push_back(from.to_string());
        visited.insert(from.to_string());
        while let Some(cur) = queue.pop_front() {
            if let Some(neighbors) = self.adjacency.get(&cur) {
                for nb in neighbors {
                    if visited.contains(nb) {
                        continue;
                    }
                    visited.insert(nb.clone());
                    prev.insert(nb.clone(), cur.clone());
                    if nb == to {
                        let mut path = Vec::new();
                        let mut node = to.to_string();
                        while let Some(p) = prev.get(&node) {
                            path.push(node.clone());
                            node = p.clone();
                        }
                        path.push(from.to_string());
                        path.reverse();
                        return path;
                    }
                    queue.push_back(nb.clone());
                }
            }
        }
        Vec::new()
    }

    /// Community detection via label propagation. Returns node id groups.
    pub fn communities(&self) -> Vec<Vec<String>> {
        let mut labels: HashMap<String, String> =
            self.nodes.keys().map(|k| (k.clone(), k.clone())).collect();
        if labels.is_empty() {
            return Vec::new();
        }
        let mut order: Vec<String> = self.nodes.keys().cloned().collect();
        order.sort();
        for _ in 0..15 {
            let mut changed = false;
            for node in &order {
                let mut counts: HashMap<String, usize> = HashMap::new();
                let mut consider = |nb: &str| {
                    if let Some(l) = labels.get(nb) {
                        *counts.entry(l.clone()).or_insert(0) += 1;
                    }
                };
                if let Some(succs) = self.adjacency.get(node) {
                    for s in succs {
                        consider(s);
                    }
                }
                if let Some(preds) = self.reverse.get(node) {
                    for p in preds {
                        consider(p);
                    }
                }
                if counts.is_empty() {
                    continue;
                }
                let best = counts
                    .into_iter()
                    .max_by_key(|(_, c)| *c)
                    .map(|(l, _)| l)
                    .unwrap();
                if labels.get(node) != Some(&best) {
                    labels.insert(node.clone(), best);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for node in &order {
            let l = labels[node].clone();
            groups.entry(l).or_default().push(node.clone());
        }
        let mut result: Vec<Vec<String>> = groups.into_values().collect();
        for g in &mut result {
            g.sort();
        }
        result.sort();
        result
    }

    /// Return the ids of the highest-PageRank nodes (top of the distribution).
    pub fn god_nodes(&self) -> Vec<String> {
        let pr = self.pagerank();
        if pr.is_empty() {
            return Vec::new();
        }
        let top = pr[0].1;
        pr.into_iter()
            .take_while(|(_, s)| (*s - top).abs() < 1e-12)
            .map(|(id, _)| id)
            .collect()
    }

    /// Save the graph as JSON to the given path.
    pub fn save(&self, path: &Path) -> Result<(), GraphMemoryError> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load a graph from a JSON file.
    pub fn load(path: &Path) -> Result<Self, GraphMemoryError> {
        let data = std::fs::read_to_string(path)?;
        let g: GraphMemory = serde_json::from_str(&data)?;
        Ok(g)
    }

    /// Compute aggregate statistics about the graph.
    pub fn stats(&self) -> GraphStats {
        let node_count = self.nodes.len();
        let edge_count = self.edges.len();
        let max_edges = if node_count > 1 {
            (node_count * (node_count - 1)) as f64
        } else {
            0.0
        };
        let density = if max_edges > 0.0 {
            edge_count as f64 / max_edges
        } else {
            0.0
        };
        let avg_degree = if node_count > 0 {
            (edge_count as f64) / node_count as f64
        } else {
            0.0
        };
        let mut type_distribution: HashMap<String, usize> = HashMap::new();
        for n in self.nodes.values() {
            *type_distribution
                .entry(n.node_type.as_str().to_string())
                .or_insert(0) += 1;
        }
        GraphStats {
            node_count,
            edge_count,
            density,
            avg_degree,
            type_distribution,
        }
    }

    fn remove_edge(&mut self, source: &str, target: &str, relation: &EdgeRelation) {
        self.edges
            .retain(|e| !(e.source == source && e.target == target && e.relation == *relation));
        if let Some(v) = self.adjacency.get_mut(source) {
            v.retain(|t| t != target);
        }
        if let Some(v) = self.reverse.get_mut(target) {
            v.retain(|s| s != source);
        }
    }

    fn remove_node(&mut self, id: &str) {
        self.nodes.remove(id);
        if let Some(succs) = self.adjacency.remove(id) {
            for s in &succs {
                if let Some(v) = self.reverse.get_mut(s) {
                    v.retain(|x| x != id);
                }
            }
        }
        if let Some(preds) = self.reverse.remove(id) {
            for p in &preds {
                if let Some(v) = self.adjacency.get_mut(p) {
                    v.retain(|x| x != id);
                }
            }
        }
        self.edges.retain(|e| e.source != id && e.target != id);
    }
}

/// A single turn in an agent conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: String,
    pub content: String,
}

/// The result of extracting memory from a conversation or scan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub nodes: Vec<MemoryNode>,
    pub edges: Vec<MemoryEdge>,
}

impl ExtractionResult {
    fn push_node(&mut self, node: MemoryNode) -> String {
        let id = node.id.clone();
        self.nodes.push(node);
        id
    }
}

/// Extracts memory nodes and edges from agent conversations via keyword matching.
#[derive(Debug, Default)]
pub struct ConversationExtractor;

impl ConversationExtractor {
    /// Create a new conversation extractor.
    pub fn new() -> Self {
        Self
    }

    /// Extract concepts, decisions, patterns, bugs, and features from a conversation.
    pub fn extract(&self, conversation: &[ConversationTurn]) -> ExtractionResult {
        let mut result = ExtractionResult::default();
        let now = Utc::now();
        for (i, turn) in conversation.iter().enumerate() {
            let content = turn.content.to_lowercase();
            let loc = format!("turn:{}", i);
            if content.contains("decided to") || content.contains("we decided") {
                let label = extract_snippet(&turn.content, "decided");
                result.push_node(MemoryNode {
                    id: String::new(),
                    label,
                    node_type: NodeType::Decision,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["decision".to_string()],
                    created_at: now,
                });
            }
            if content.contains("fixed bug")
                || content.contains("fixes bug")
                || content.contains("bug fix")
            {
                let bug_label = extract_snippet(&turn.content, "bug");
                let bug_id = result.push_node(MemoryNode {
                    id: String::new(),
                    label: bug_label,
                    node_type: NodeType::Bug,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["bug".to_string()],
                    created_at: now,
                });
                let fix_id = result.push_node(MemoryNode {
                    id: String::new(),
                    label: format!(
                        "Fix for {}",
                        turn.content.chars().take(40).collect::<String>()
                    ),
                    node_type: NodeType::Feature,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["fix".to_string()],
                    created_at: now,
                });
                result.edges.push(MemoryEdge {
                    source: bug_id,
                    target: fix_id,
                    relation: EdgeRelation::FixedBy,
                    confidence: 0.8,
                    source_file: None,
                });
            }
            if content.contains("implemented") || content.contains("implements") {
                let label = extract_snippet(&turn.content, "implemented");
                result.push_node(MemoryNode {
                    id: String::new(),
                    label,
                    node_type: NodeType::Feature,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["feature".to_string()],
                    created_at: now,
                });
            }
            if content.contains("pattern") {
                let label = extract_snippet(&turn.content, "pattern");
                result.push_node(MemoryNode {
                    id: String::new(),
                    label,
                    node_type: NodeType::Pattern,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["pattern".to_string()],
                    created_at: now,
                });
            }
            if content.contains("concept") || content.contains("idea") {
                let label = extract_snippet(&turn.content, "concept");
                result.push_node(MemoryNode {
                    id: String::new(),
                    label,
                    node_type: NodeType::Concept,
                    description: turn.content.clone(),
                    source_file: None,
                    source_location: Some(loc.clone()),
                    tags: vec!["concept".to_string()],
                    created_at: now,
                });
            }
        }
        result
    }
}

fn extract_snippet(content: &str, keyword: &str) -> String {
    let lower = content.to_lowercase();
    if let Some(idx) = lower.find(keyword) {
        let start = idx;
        let end = (start + 60).min(content.len());
        content[start..end].trim().to_string()
    } else {
        content.chars().take(60).collect()
    }
}

/// Scans a workspace and builds a structural graph of files, functions, and classes.
#[derive(Debug)]
pub struct CodebaseScanner {
    workspace: PathBuf,
}

impl CodebaseScanner {
    /// Create a scanner rooted at the given workspace.
    pub fn new(workspace: PathBuf) -> Self {
        CodebaseScanner { workspace }
    }

    /// Scan all `.rs`, `.ts`, `.py`, and `.go` files in the workspace.
    pub fn scan(&self) -> Result<ExtractionResult, GraphMemoryError> {
        let mut files = Vec::new();
        self.collect_files(&self.workspace, &mut files)?;
        self.scan_incremental(&files)
    }

    /// Scan only the provided changed files.
    pub fn scan_incremental(
        &self,
        changed_files: &[PathBuf],
    ) -> Result<ExtractionResult, GraphMemoryError> {
        let mut result = ExtractionResult::default();
        let now = Utc::now();
        let mut file_ids: HashMap<String, String> = HashMap::new();
        for path in changed_files {
            if !path.exists() {
                continue;
            }
            let rel = path
                .strip_prefix(&self.workspace)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let file_id = next_node_id();
            file_ids.insert(rel.clone(), file_id.clone());
            result.push_node(MemoryNode {
                id: file_id.clone(),
                label: rel.clone(),
                node_type: NodeType::File,
                description: format!("Source file: {}", rel),
                source_file: Some(rel.clone()),
                source_location: None,
                tags: vec![language_tag(path)],
                created_at: now,
            });
            let defs = extract_definitions(&content, path);
            for def in defs {
                let def_id = next_node_id();
                result.edges.push(MemoryEdge {
                    source: file_id.clone(),
                    target: def_id.clone(),
                    relation: EdgeRelation::Contains,
                    confidence: 1.0,
                    source_file: Some(rel.clone()),
                });
                result.push_node(MemoryNode {
                    id: def_id,
                    label: def.name.clone(),
                    node_type: def.kind,
                    description: format!("{} defined in {}", def.name, rel),
                    source_file: Some(rel.clone()),
                    source_location: Some(def.location.clone()),
                    tags: Vec::new(),
                    created_at: now,
                });
            }
            for imp in extract_imports(&content, path) {
                if let Some(target_id) = file_ids.get(&imp) {
                    if target_id != &file_id {
                        result.edges.push(MemoryEdge {
                            source: file_id.clone(),
                            target: target_id.clone(),
                            relation: EdgeRelation::Imports,
                            confidence: 0.9,
                            source_file: Some(rel.clone()),
                        });
                    }
                }
            }
        }
        Ok(result)
    }

    fn collect_files(&self, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), GraphMemoryError> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name.starts_with('.')
                    || name == "target"
                    || name == "node_modules"
                    || name == "vendor"
                {
                    continue;
                }
                self.collect_files(&path, out)?;
            } else if is_source_file(&path) {
                out.push(path);
            }
        }
        Ok(())
    }
}

fn is_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs") | Some("ts") | Some("py") | Some("go")
    )
}

fn language_tag(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust",
        Some("ts") => "typescript",
        Some("py") => "python",
        Some("go") => "go",
        _ => "unknown",
    }
    .to_string()
}

struct Definition {
    name: String,
    kind: NodeType,
    location: String,
}

fn extract_definitions(content: &str, path: &Path) -> Vec<Definition> {
    let mut defs = Vec::new();
    let lang = language_tag(path);
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        let name = match lang.as_str() {
            "rust" => extract_rust_def(trimmed),
            "typescript" => extract_ts_def(trimmed),
            "python" => extract_py_def(trimmed),
            "go" => extract_go_def(trimmed),
            _ => None,
        };
        if let Some((name, kind)) = name {
            defs.push(Definition {
                name,
                kind,
                location: format!("line:{}", i + 1),
            });
        }
    }
    defs
}

fn extract_rust_def(line: &str) -> Option<(String, NodeType)> {
    let no_pub = line.strip_prefix("pub ").unwrap_or(line);
    let no_pub = no_pub.strip_prefix("pub(crate) ").unwrap_or(no_pub);
    if let Some(rest) = no_pub.strip_prefix("fn ") {
        return Some((take_ident(rest), NodeType::Function));
    }
    if let Some(rest) = no_pub.strip_prefix("struct ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    if let Some(rest) = no_pub.strip_prefix("enum ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    if let Some(rest) = no_pub.strip_prefix("trait ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    if let Some(rest) = no_pub.strip_prefix("mod ") {
        return Some((take_ident(rest), NodeType::Module));
    }
    None
}

fn extract_ts_def(line: &str) -> Option<(String, NodeType)> {
    if let Some(rest) = line.strip_prefix("export ") {
        if let Some(r) = rest.strip_prefix("function ") {
            return Some((take_ident(r), NodeType::Function));
        }
        if let Some(r) = rest.strip_prefix("class ") {
            return Some((take_ident(r), NodeType::Class));
        }
    }
    if let Some(rest) = line.strip_prefix("function ") {
        return Some((take_ident(rest), NodeType::Function));
    }
    if let Some(rest) = line.strip_prefix("class ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    None
}

fn extract_py_def(line: &str) -> Option<(String, NodeType)> {
    if let Some(rest) = line.strip_prefix("def ") {
        return Some((take_ident(rest), NodeType::Function));
    }
    if let Some(rest) = line.strip_prefix("class ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    None
}

fn extract_go_def(line: &str) -> Option<(String, NodeType)> {
    if let Some(rest) = line.strip_prefix("func ") {
        if let Some(rest) = rest.strip_prefix("(") {
            if let Some(idx) = rest.find(')') {
                let after = &rest[idx + 1..].trim_start();
                if let Some(r) = after.strip_prefix(" ") {
                    return Some((take_ident(r), NodeType::Function));
                }
                return Some((take_ident(after), NodeType::Function));
            }
        }
        return Some((take_ident(rest), NodeType::Function));
    }
    if let Some(rest) = line.strip_prefix("type ") {
        return Some((take_ident(rest), NodeType::Class));
    }
    None
}

fn take_ident(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            break;
        }
    }
    out
}

fn extract_imports(content: &str, path: &Path) -> Vec<String> {
    let mut imports = Vec::new();
    let lang = language_tag(path);
    for line in content.lines() {
        let trimmed = line.trim_start();
        match lang.as_str() {
            "rust" => {
                if let Some(rest) = trimmed.strip_prefix("mod ") {
                    let name = take_ident(rest);
                    if !name.is_empty() {
                        imports.push(format!("{}.rs", name));
                    }
                }
                if let Some(rest) = trimmed.strip_prefix("use ") {
                    if let Some(last) = rest
                        .split(|c: char| !c.is_alphanumeric() && c != '_')
                        .next_back()
                    {
                        if !last.is_empty() {
                            imports.push(format!("{}.rs", last));
                        }
                    }
                }
            }
            "python" => {
                if let Some(rest) = trimmed.strip_prefix("from ") {
                    if let Some(idx) = rest.find(" import ") {
                        imports.push(rest[..idx].trim().to_string());
                    }
                } else if let Some(rest) = trimmed.strip_prefix("import ") {
                    imports.push(rest.split_whitespace().next().unwrap_or("").to_string());
                }
            }
            "typescript" => {
                if let Some(rest) = trimmed.strip_prefix("import ") {
                    if let Some(idx) = rest.find("from ") {
                        let target = rest[idx + 5..]
                            .trim()
                            .trim_matches(|c: char| c == '"' || c == '\'');
                        imports.push(target.to_string());
                    }
                }
            }
            "go" if trimmed.starts_with("import") => {
                let quoted: Vec<&str> = trimmed.split('"').collect();
                if quoted.len() >= 3 {
                    imports.push(quoted[1].to_string());
                }
            }
            _ => {}
        }
    }
    imports
}

/// Result of a dream consolidation pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsolidationResult {
    pub merged_count: usize,
    pub pruned_count: usize,
    pub new_edges: usize,
}

/// Nightly memory consolidation: merges similar nodes, prunes low-confidence edges,
/// and infers new edges between co-occurring nodes.
#[derive(Debug, Default)]
pub struct DreamConsolidator;

impl DreamConsolidator {
    /// Create a new dream consolidator.
    pub fn new() -> Self {
        Self
    }

    /// Consolidate the graph in place.
    pub fn consolidate(&self, graph: &mut GraphMemory) -> ConsolidationResult {
        let mut result = ConsolidationResult::default();
        let confidence_threshold = 0.3;
        let pruned: Vec<(String, String, EdgeRelation)> = graph
            .edges
            .iter()
            .filter(|e| e.confidence < confidence_threshold)
            .map(|e| (e.source.clone(), e.target.clone(), e.relation.clone()))
            .collect();
        result.pruned_count = pruned.len();
        for (s, t, r) in &pruned {
            graph.remove_edge(s, t, r);
        }

        let mut by_label: HashMap<String, Vec<String>> = HashMap::new();
        let labels: Vec<(String, String, NodeType)> = graph
            .nodes
            .iter()
            .map(|(id, n)| (id.clone(), n.label.to_lowercase(), n.node_type.clone()))
            .collect();
        for (id, label, ty) in &labels {
            if let NodeType::Other = ty {
                continue;
            }
            by_label.entry(label.clone()).or_default().push(id.clone());
        }
        let merges: Vec<(String, (String, Vec<String>))> = by_label
            .into_iter()
            .filter(|(_, ids)| ids.len() > 1)
            .map(|(label, ids)| {
                let mut ids = ids;
                ids.sort();
                let keeper = ids.remove(0);
                (label, (keeper, ids))
            })
            .collect();
        for (_, (keeper, duplicates)) in &merges {
            for dup in duplicates {
                let succs: Vec<String> = graph.adjacency.get(dup).cloned().unwrap_or_default();
                for s in succs {
                    if s != *dup && s != *keeper && graph.nodes.contains_key(&s) {
                        let already = graph
                            .adjacency
                            .get(keeper)
                            .map(|v| v.contains(&s))
                            .unwrap_or(false);
                        if !already {
                            let _ = graph.add_edge(MemoryEdge {
                                source: keeper.clone(),
                                target: s,
                                relation: EdgeRelation::RelatedTo,
                                confidence: 0.5,
                                source_file: None,
                            });
                        }
                    }
                }
                graph.remove_node(dup);
                result.merged_count += 1;
            }
        }

        let nodes: Vec<String> = graph.nodes.keys().cloned().collect();
        for i in 0..nodes.len() {
            for j in (i + 1)..nodes.len() {
                let a = &nodes[i];
                let b = &nodes[j];
                let a_succs: HashSet<&String> =
                    graph.adjacency.get(a).into_iter().flatten().collect();
                let b_succs: HashSet<&String> =
                    graph.adjacency.get(b).into_iter().flatten().collect();
                let common = a_succs.intersection(&b_succs).count();
                if common >= 2 {
                    let exists = graph.edges.iter().any(|e| {
                        (e.source == *a && e.target == *b) || (e.source == *b && e.target == *a)
                    });
                    if !exists {
                        let _ = graph.add_edge(MemoryEdge {
                            source: a.clone(),
                            target: b.clone(),
                            relation: EdgeRelation::RelatedTo,
                            confidence: 0.4,
                            source_file: None,
                        });
                        result.new_edges += 1;
                    }
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn node(label: &str, ty: NodeType, desc: &str) -> MemoryNode {
        MemoryNode {
            id: String::new(),
            label: label.to_string(),
            node_type: ty,
            description: desc.to_string(),
            source_file: None,
            source_location: None,
            tags: Vec::new(),
            created_at: Utc::now(),
        }
    }

    fn edge(s: &str, t: &str, r: EdgeRelation) -> MemoryEdge {
        MemoryEdge {
            source: s.to_string(),
            target: t.to_string(),
            relation: r,
            confidence: 0.9,
            source_file: None,
        }
    }

    #[test]
    fn test_node_edge_addition() {
        let mut g = GraphMemory::new();
        let a = g.add_node(node("alpha", NodeType::Function, "first"));
        let b = g.add_node(node("beta", NodeType::Function, "second"));
        assert!(g.add_edge(edge(&a, &b, EdgeRelation::Calls)).is_ok());
        assert_eq!(g.get_node(&a).unwrap().label, "alpha");
        let nb = g.get_neighbors(&a);
        assert_eq!(nb.len(), 1);
        assert_eq!(nb[0].label, "beta");
        assert!(g
            .add_edge(edge("missing", &b, EdgeRelation::Calls))
            .is_err());
    }

    #[test]
    fn test_search_keyword() {
        let mut g = GraphMemory::new();
        g.add_node(node("parse_tokens", NodeType::Function, "tokenizes input"));
        g.add_node(node("render_html", NodeType::Function, "renders output"));
        let r = g.search("token");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].label, "parse_tokens");
        let r2 = g.search("render");
        assert_eq!(r2.len(), 1);
        assert_eq!(r2[0].label, "render_html");
    }

    #[test]
    fn test_pagerank_convergence() {
        let mut g = GraphMemory::new();
        let a = g.add_node(node("a", NodeType::Concept, "a"));
        let b = g.add_node(node("b", NodeType::Concept, "b"));
        let c = g.add_node(node("c", NodeType::Concept, "c"));
        g.add_edge(edge(&a, &b, EdgeRelation::RelatedTo)).unwrap();
        g.add_edge(edge(&b, &c, EdgeRelation::RelatedTo)).unwrap();
        g.add_edge(edge(&c, &a, EdgeRelation::RelatedTo)).unwrap();
        let pr = g.pagerank();
        assert_eq!(pr.len(), 3);
        let total: f64 = pr.iter().map(|(_, s)| s).sum();
        assert!((total - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_shortest_path() {
        let mut g = GraphMemory::new();
        let a = g.add_node(node("a", NodeType::Concept, "a"));
        let b = g.add_node(node("b", NodeType::Concept, "b"));
        let c = g.add_node(node("c", NodeType::Concept, "c"));
        g.add_edge(edge(&a, &b, EdgeRelation::RelatedTo)).unwrap();
        g.add_edge(edge(&b, &c, EdgeRelation::RelatedTo)).unwrap();
        let path = g.shortest_path(&a, &c);
        assert_eq!(path, vec![a.clone(), b.clone(), c.clone()]);
        let none = g.shortest_path(&c, &a);
        assert!(none.is_empty());
    }

    #[test]
    fn test_community_detection() {
        let mut g = GraphMemory::new();
        let a = g.add_node(node("a", NodeType::Concept, "a"));
        let b = g.add_node(node("b", NodeType::Concept, "b"));
        let c = g.add_node(node("c", NodeType::Concept, "c"));
        let d = g.add_node(node("d", NodeType::Concept, "d"));
        g.add_edge(edge(&a, &b, EdgeRelation::RelatedTo)).unwrap();
        g.add_edge(edge(&b, &a, EdgeRelation::RelatedTo)).unwrap();
        g.add_edge(edge(&c, &d, EdgeRelation::RelatedTo)).unwrap();
        g.add_edge(edge(&d, &c, EdgeRelation::RelatedTo)).unwrap();
        let comms = g.communities();
        assert!(comms.len() >= 2);
        let total: usize = comms.iter().map(|c| c.len()).sum();
        assert_eq!(total, 4);
    }

    #[test]
    fn test_god_nodes() {
        let mut g = GraphMemory::new();
        let a = g.add_node(node("hub", NodeType::Concept, "central"));
        let b = g.add_node(node("spoke1", NodeType::Concept, "leaf"));
        let c = g.add_node(node("spoke2", NodeType::Concept, "leaf"));
        let d = g.add_node(node("spoke3", NodeType::Concept, "leaf"));
        g.add_edge(edge(&b, &a, EdgeRelation::RelatedTo)).unwrap();
        g.add_edge(edge(&c, &a, EdgeRelation::RelatedTo)).unwrap();
        g.add_edge(edge(&d, &a, EdgeRelation::RelatedTo)).unwrap();
        g.add_edge(edge(&a, &b, EdgeRelation::RelatedTo)).unwrap();
        let gods = g.god_nodes();
        assert!(!gods.is_empty());
        let pr = g.pagerank();
        assert_eq!(gods[0], pr[0].0);
        assert_eq!(gods[0], a);
    }

    #[test]
    fn test_save_load_roundtrip() {
        let mut g = GraphMemory::new();
        let a = g.add_node(node("alpha", NodeType::Function, "first"));
        let b = g.add_node(node("beta", NodeType::Function, "second"));
        g.add_edge(edge(&a, &b, EdgeRelation::Calls)).unwrap();
        let tmp = std::env::temp_dir().join("graph_memory_test.json");
        g.save(&tmp).unwrap();
        let loaded = GraphMemory::load(&tmp).unwrap();
        assert_eq!(loaded.nodes.len(), 2);
        assert_eq!(loaded.edges.len(), 1);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_conversation_extraction() {
        let turns = vec![
            ConversationTurn {
                role: "assistant".to_string(),
                content: "We decided to use a graph-based memory store".to_string(),
            },
            ConversationTurn {
                role: "assistant".to_string(),
                content: "Fixed bug in the pagerank calculation".to_string(),
            },
            ConversationTurn {
                role: "assistant".to_string(),
                content: "We implemented the dream consolidation pattern".to_string(),
            },
        ];
        let ext = ConversationExtractor::new();
        let result = ext.extract(&turns);
        assert!(result
            .nodes
            .iter()
            .any(|n| n.node_type == NodeType::Decision));
        assert!(result.nodes.iter().any(|n| n.node_type == NodeType::Bug));
        assert!(result
            .nodes
            .iter()
            .any(|n| n.node_type == NodeType::Feature));
        assert!(result
            .nodes
            .iter()
            .any(|n| n.node_type == NodeType::Pattern));
        assert!(result
            .edges
            .iter()
            .any(|e| e.relation == EdgeRelation::FixedBy));
    }

    #[test]
    fn test_codebase_scanner_rust() {
        let dir = std::env::temp_dir().join("graph_memory_scan_test");
        std::fs::create_dir_all(&dir).unwrap();
        let rs = dir.join("lib.rs");
        let mut f = std::fs::File::create(&rs).unwrap();
        writeln!(f, "pub fn hello() {{}}").unwrap();
        writeln!(f, "struct Foo;").unwrap();
        writeln!(f, "mod bar;").unwrap();
        let scanner = CodebaseScanner::new(dir.clone());
        let result = scanner.scan().unwrap();
        assert!(result.nodes.iter().any(|n| n.node_type == NodeType::File));
        assert!(result
            .nodes
            .iter()
            .any(|n| n.node_type == NodeType::Function && n.label == "hello"));
        assert!(result
            .nodes
            .iter()
            .any(|n| n.node_type == NodeType::Class && n.label == "Foo"));
        assert!(result
            .edges
            .iter()
            .any(|e| e.relation == EdgeRelation::Contains));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_dream_consolidation_merge() {
        let mut g = GraphMemory::new();
        let a = g.add_node(node("duplicate", NodeType::Concept, "first"));
        let b = g.add_node(node("duplicate", NodeType::Concept, "second"));
        let c = g.add_node(node("unique", NodeType::Concept, "third"));
        g.add_edge(edge(&a, &c, EdgeRelation::RelatedTo)).unwrap();
        g.add_edge(edge(&b, &c, EdgeRelation::RelatedTo)).unwrap();
        let consolidator = DreamConsolidator::new();
        let result = consolidator.consolidate(&mut g);
        assert!(result.merged_count >= 1);
        assert!(g.nodes.len() < 3);
    }

    #[test]
    fn test_dream_consolidation_prune() {
        let mut g = GraphMemory::new();
        let a = g.add_node(node("a", NodeType::Concept, "a"));
        let b = g.add_node(node("b", NodeType::Concept, "b"));
        g.add_edge(MemoryEdge {
            source: a.clone(),
            target: b,
            relation: EdgeRelation::RelatedTo,
            confidence: 0.1,
            source_file: None,
        });
        let consolidator = DreamConsolidator::new();
        let result = consolidator.consolidate(&mut g);
        assert_eq!(result.pruned_count, 1);
        assert_eq!(g.edges.len(), 0);
    }

    #[test]
    fn test_graph_stats() {
        let mut g = GraphMemory::new();
        g.add_node(node("a", NodeType::Function, "a"));
        g.add_node(node("b", NodeType::Class, "b"));
        let a = g.nodes.keys().next().cloned().unwrap();
        let b = g.nodes.keys().last().cloned().unwrap();
        g.add_edge(edge(&a, &b, EdgeRelation::Calls)).unwrap();
        let stats = g.stats();
        assert_eq!(stats.node_count, 2);
        assert_eq!(stats.edge_count, 1);
        assert!(stats.density > 0.0);
        assert_eq!(stats.avg_degree, 0.5);
        assert!(stats.type_distribution.contains_key("Function"));
        assert!(stats.type_distribution.contains_key("Class"));
    }
}
