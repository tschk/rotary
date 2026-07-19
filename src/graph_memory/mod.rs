//! Knowledge graph memory: nodes, edges, extraction, scan, dream consolidation.

mod dream;
mod extract;
mod graph;
mod scan;

pub use dream::*;
pub use extract::*;
pub use graph::*;
pub use scan::*;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
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
        })
        .unwrap();
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
