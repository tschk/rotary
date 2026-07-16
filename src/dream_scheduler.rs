//! Dream scheduler — runs [`DreamConsolidator`] on a periodic cycle.
//!
//! Mirrors the pattern omi uses for `gbrain::run_nightly_cycle`: a long-running
//! background task ticks on a fixed interval and consolidates the graph memory
//! in place. The synchronous surface (`should_run` / `run_cycle`) is always
//! available; the tokio-driven `start_background` loop is gated behind the
//! `ipc` feature, matching the rest of the crate.

use chrono::{DateTime, Duration, Utc};

use crate::graph_memory::{ConsolidationResult, DreamConsolidator, GraphMemory, GraphStats};

/// Configuration for the [`DreamScheduler`].
#[derive(Debug, Clone)]
pub struct DreamScheduleConfig {
    /// Seconds between dream cycles. Default 14400 (4 hours), matching omi.
    pub interval_secs: u64,
    /// Whether the scheduler is permitted to run cycles. Default `false`.
    pub enabled: bool,
    /// Soft cap on graph node count; cycles are skipped once exceeded.
    pub max_graph_nodes: usize,
}

impl Default for DreamScheduleConfig {
    fn default() -> Self {
        Self {
            interval_secs: 14_400,
            enabled: false,
            max_graph_nodes: 10_000,
        }
    }
}

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

/// Schedules [`DreamConsolidator`] runs on a fixed interval.
pub struct DreamScheduler {
    config: DreamScheduleConfig,
    last_run: Option<DateTime<Utc>>,
    run_count: u64,
    consolidator: DreamConsolidator,
}

impl DreamScheduler {
    /// Create a new scheduler with the supplied config.
    pub fn new(config: DreamScheduleConfig) -> Self {
        Self {
            config,
            last_run: None,
            run_count: 0,
            consolidator: DreamConsolidator::new(),
        }
    }

    /// Returns `true` when the scheduler is enabled and enough time has
    /// elapsed since the last run (or no run has happened yet).
    pub fn should_run(&self) -> bool {
        if !self.config.enabled {
            return false;
        }
        match self.last_run {
            None => true,
            Some(last) => Utc::now() - last >= Duration::seconds(self.config.interval_secs as i64),
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

    /// Number of cycles executed by this scheduler.
    pub fn run_count(&self) -> u64 {
        self.run_count
    }

    /// Timestamp of the most recent cycle, if any.
    pub fn last_run(&self) -> Option<DateTime<Utc>> {
        self.last_run
    }

    /// Projected timestamp of the next cycle (`last_run + interval_secs`).
    pub fn next_run(&self) -> Option<DateTime<Utc>> {
        self.last_run
            .map(|last| last + Duration::seconds(self.config.interval_secs as i64))
    }

    /// Borrow the active configuration.
    pub fn config(&self) -> &DreamScheduleConfig {
        &self.config
    }
}

#[cfg(feature = "ipc")]
impl DreamScheduler {
    /// Spawn a tokio task that ticks on `config.interval_secs` and consolidates
    /// the shared graph. The task runs until the returned handle is aborted.
    ///
    /// Requires the `ipc` feature (which enables tokio).
    pub fn start_background(
        &self,
        graph: std::sync::Arc<parking_lot::RwLock<GraphMemory>>,
    ) -> tokio::task::JoinHandle<()> {
        let interval_secs = self.config.interval_secs;
        let max_graph_nodes = self.config.max_graph_nodes;
        let consolidator = DreamConsolidator::new();

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
            // Skip the immediate first tick so the first cycle waits a full interval.
            ticker.tick().await;

            loop {
                ticker.tick().await;

                let mut graph = graph.write();
                if graph.stats().node_count > max_graph_nodes {
                    tracing::info!(
                        "dream scheduler: graph exceeds max_graph_nodes ({}), skipping cycle",
                        max_graph_nodes
                    );
                    continue;
                }

                let result = consolidator.consolidate(&mut graph);
                tracing::info!(
                    "dream scheduler: cycle complete — merged={} pruned={} new_edges={}",
                    result.merged_count,
                    result.pruned_count,
                    result.new_edges
                );
            }
        })
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
    fn test_should_run_first_time() {
        let config = DreamScheduleConfig {
            enabled: true,
            ..DreamScheduleConfig::default()
        };
        let scheduler = DreamScheduler::new(config);
        assert!(scheduler.should_run());
    }

    #[test]
    fn test_should_run_after_interval() {
        let config = DreamScheduleConfig {
            enabled: true,
            interval_secs: 1,
            ..DreamScheduleConfig::default()
        };
        let mut scheduler = DreamScheduler::new(config);
        let mut graph = populated_graph();
        scheduler.run_cycle(&mut graph);

        // Immediately after a run, should_run is false.
        assert!(!scheduler.should_run());

        // Simulate the passage of one interval by backdating last_run.
        scheduler.last_run = Some(Utc::now() - Duration::seconds(2));
        assert!(scheduler.should_run());
    }

    #[test]
    fn test_run_cycle_executes() {
        let config = DreamScheduleConfig {
            enabled: true,
            ..DreamScheduleConfig::default()
        };
        let mut scheduler = DreamScheduler::new(config);
        let mut graph = populated_graph();

        let result = scheduler.run_cycle(&mut graph);
        // The low-confidence edge (0.1) should be pruned.
        assert_eq!(result.pruned_count, 1);
        assert!(scheduler.last_run().is_some());
    }

    #[test]
    fn test_run_count_increments() {
        let config = DreamScheduleConfig {
            enabled: true,
            ..DreamScheduleConfig::default()
        };
        let mut scheduler = DreamScheduler::new(config);
        let mut graph = populated_graph();

        assert_eq!(scheduler.run_count(), 0);
        scheduler.run_cycle(&mut graph);
        assert_eq!(scheduler.run_count(), 1);
        scheduler.run_cycle(&mut graph);
        assert_eq!(scheduler.run_count(), 2);
    }

    #[test]
    fn test_dream_report() {
        let config = DreamScheduleConfig {
            enabled: true,
            ..DreamScheduleConfig::default()
        };
        let mut scheduler = DreamScheduler::new(config);
        let mut graph = populated_graph();

        let report = scheduler.run_cycle_with_report(&mut graph);
        assert_eq!(report.run_number, 1);
        assert_eq!(report.result.pruned_count, 1);
        assert_eq!(report.graph_stats.node_count, 2);
        assert_eq!(report.graph_stats.edge_count, 0);

        // next_run should be last_run + interval_secs.
        let last = scheduler.last_run().unwrap();
        let next = scheduler.next_run().unwrap();
        assert_eq!(next, last + Duration::seconds(14_400));
    }
}
