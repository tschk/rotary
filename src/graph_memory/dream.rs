use super::graph::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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
