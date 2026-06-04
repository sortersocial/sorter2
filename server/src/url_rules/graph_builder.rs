//! Declarative construction of the URL graph with build-time link validation.

use std::collections::HashMap;

use super::graph::{CanonicalFn, Edge, EdgePattern, Graph, Node};

pub struct GraphBuilder {
    nodes: HashMap<&'static str, Node>,
    current: Option<&'static str>,
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            current: None,
        }
    }

    pub fn node(mut self, id: &'static str) -> Self {
        self.nodes.entry(id).or_insert_with(Node::empty);
        self.current = Some(id);
        self
    }

    pub fn canonical(mut self, f: CanonicalFn) -> Self {
        let id = self.current.expect("canonical() without node()");
        self.nodes.get_mut(id).expect("node missing").canonical = f;
        self
    }

    pub fn parent(mut self, parent_id: &'static str) -> Self {
        let id = self.current.expect("parent() without node()");
        self.nodes.get_mut(id).expect("node missing").parent = Some(parent_id);
        self
    }

    pub fn edge(mut self, pattern: EdgePattern, target: &'static str) -> Self {
        let id = self.current.expect("edge() without node()");
        self.nodes
            .get_mut(id)
            .expect("node missing")
            .edges
            .push(Edge { pattern, target });
        self
    }

    pub fn build(self) -> Graph {
        for (id, node) in &self.nodes {
            if let Some(parent) = node.parent {
                assert!(
                    self.nodes.contains_key(parent),
                    "node {id}: parent {parent} does not exist"
                );
            }
            for edge in &node.edges {
                if !matches!(
                    edge.pattern,
                    EdgePattern::AbsorbAny | EdgePattern::AbsorbIf(_)
                ) {
                    assert!(
                        self.nodes.contains_key(edge.target),
                        "node {id}: edge target {} does not exist",
                        edge.target
                    );
                }
            }
        }
        Graph {
            nodes: self.nodes,
        }
    }
}
