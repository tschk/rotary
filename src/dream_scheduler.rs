//! Dream cycle — runs [`DreamConsolidator`] and reports the result.
//!
//! This module exposes the *capability* of running a consolidation cycle.
//! Scheduling (when to run, how often, whether it's enabled) is the host's
//! concern — omi wires this into its `assistants.rs` scheduler, telekinesis
//! may call it on a slash command, etc.

use chrono::{DateTime, Utc};

use crate::graph_memory::{ConsolidationResult, DreamConsolidator, GraphMemory, GraphStats};

/// A report produced by a single dream cycle.
#[derive(Debug, Clone)]
pub struct DreamReport {
    /// 1-indexed run number for this scheduler.
    pub run_number: u64,
    /// When the cycle executed.
    pub timestamp: DateTime<Utc>,
    /// Consolidation outcome.
    pub result: ConsolidationResult,
    /// Graph statistics captured after consolidation.
    pub graph_stats: GraphStats,
}

/// Runs [`DreamConsolidator`] cycles and tracks bookkeeping.
///
/// The host decides *when* to call `run_cycle` — this struct only
/// provides the capability and tracks how many cycles have run.
pub struct DreamScheduler {
    last_run: Option<DateTime<Utc>>,
    run_count: u64,
    consolidator: DreamConsolidator,
}

impl Default for DreamScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl DreamScheduler {
    /// Create a new dream cycle runner.
    pub fn new() -> Self {
        Self {
            last_run: None,
            run_count: 0,
            consolidator: DreamConsolidator::new(),
        }
    }

    /// Execute one consolidation cycle against `graph`, updating bookkeeping
    /// and returning the consolidator's result.
    pub fn run_cycle(&mut self, graph: &mut GraphMemory) -> ConsolidationResult {
        let result = self.consolidator.consolidate(graph);
        self.last_run = Some(Utc::now());
        self.run_count += 1;
        result
    }

    /// Execute one cycle and return a full [`DreamReport`] including graph
    /// statistics captured after consolidation.
    pub fn run_cycle_with_report(&mut self, graph: &mut GraphMemory) -> DreamReport {
        let result = self.run_cycle(graph);
        let graph_stats = graph.stats();
        DreamReport {
            run_number: self.run_count,
            timestamp: self.last_run.unwrap_or_else(Utc::now),
            result,
            graph_stats,
        }
    }

    /// Number of cycles executed by this runner.
    pub fn run_count(&self) -> u64 {
        self.run_count
    }

    /// Timestamp of the most recent cycle, if any.
    pub fn last_run(&self) -> Option<DateTime<Utc>> {
        self.last_run
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_memory::{EdgeRelation, MemoryEdge, MemoryNode, NodeType};

    fn populated_graph() -> GraphMemory {
        let mut graph = GraphMemory::new();
        graph.add_node(MemoryNode {
            id: "alpha".to_string(),
            label: "Alpha".to_string(),
            node_type: NodeType::Concept,
            description: String::new(),
            source_file: None,
            source_location: None,
            tags: Vec::new(),
            created_at: Utc::now(),
        });
        graph.add_node(MemoryNode {
            id: "beta".to_string(),
            label: "Beta".to_string(),
            node_type: NodeType::Concept,
            description: String::new(),
            source_file: None,
            source_location: None,
            tags: Vec::new(),
            created_at: Utc::now(),
        });
        let _ = graph.add_edge(MemoryEdge {
            source: "alpha".to_string(),
            target: "beta".to_string(),
            relation: EdgeRelation::RelatedTo,
            confidence: 0.1,
            source_file: None,
        });
        graph
    }

    #[test]
    fn test_run_cycle_executes() {
        let mut scheduler = DreamScheduler::new();
        let mut graph = populated_graph();

        let result = scheduler.run_cycle(&mut graph);
        assert_eq!(result.pruned_count, 1);
        assert!(scheduler.last_run().is_some());
    }

    #[test]
    fn test_run_count_increments() {
        let mut scheduler = DreamScheduler::new();
        let mut graph = populated_graph();

        assert_eq!(scheduler.run_count(), 0);
        scheduler.run_cycle(&mut graph);
        assert_eq!(scheduler.run_count(), 1);
        scheduler.run_cycle(&mut graph);
        assert_eq!(scheduler.run_count(), 2);
    }

    #[test]
    fn test_dream_report() {
        let mut scheduler = DreamScheduler::new();
        let mut graph = populated_graph();

        let report = scheduler.run_cycle_with_report(&mut graph);
        assert_eq!(report.run_number, 1);
        assert_eq!(report.result.pruned_count, 1);
        assert_eq!(report.graph_stats.node_count, 2);
        assert_eq!(report.graph_stats.edge_count, 0);
    }
}
