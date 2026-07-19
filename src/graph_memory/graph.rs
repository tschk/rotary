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

pub(crate) fn next_node_id() -> String {
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
    pub(crate) nodes: HashMap<String, MemoryNode>,
    pub(crate) edges: Vec<MemoryEdge>,
    pub(crate) adjacency: HashMap<String, Vec<String>>,
    pub(crate) reverse: HashMap<String, Vec<String>>,
    pub(crate) workspace: Option<PathBuf>,
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

    pub(crate) fn remove_edge(&mut self, source: &str, target: &str, relation: &EdgeRelation) {
        self.edges
            .retain(|e| !(e.source == source && e.target == target && e.relation == *relation));
        if let Some(v) = self.adjacency.get_mut(source) {
            v.retain(|t| t != target);
        }
        if let Some(v) = self.reverse.get_mut(target) {
            v.retain(|s| s != source);
        }
    }

    pub(crate) fn remove_node(&mut self, id: &str) {
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
