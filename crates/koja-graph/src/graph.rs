//! The graph type and its edge bookkeeping.

use std::collections::BTreeMap;

/// A directed graph over nodes of type `K`. An edge `from -> to`
/// means `from` depends on `to`. Nodes iterate in key order and the
/// edges of one node in insertion order.
#[derive(Clone, Debug)]
pub struct Graph<K> {
    edges: BTreeMap<K, Vec<K>>,
}

impl<K> Default for Graph<K> {
    fn default() -> Self {
        Self {
            edges: BTreeMap::new(),
        }
    }
}

impl<K: Clone + Ord> Graph<K> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `from` depends on `to`. Both nodes are added when
    /// missing. A repeated edge is ignored.
    pub fn add_edge(&mut self, from: K, to: K) {
        self.add_node(to.clone());
        let targets = self.edges.entry(from).or_default();
        if !targets.contains(&to) {
            targets.push(to);
        }
    }

    /// Add `node` with no edges. A node already present is left as
    /// it is.
    pub fn add_node(&mut self, node: K) {
        self.edges.entry(node).or_default();
    }

    /// The nodes `node` depends on, in insertion order. Empty for a
    /// node the graph does not hold.
    pub fn dependencies(&self, node: &K) -> &[K] {
        self.edges.get(node).map_or(&[], Vec::as_slice)
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Every node, in key order.
    pub fn nodes(&self) -> impl Iterator<Item = &K> {
        self.edges.keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_edge_adds_both_nodes() {
        let mut graph = Graph::new();
        graph.add_edge("a", "b");
        assert_eq!(graph.nodes().copied().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(graph.dependencies(&"a"), ["b"]);
        assert!(graph.dependencies(&"b").is_empty());
    }

    #[test]
    fn repeated_edge_is_ignored() {
        let mut graph = Graph::new();
        graph.add_edge(1, 2);
        graph.add_edge(1, 2);
        assert_eq!(graph.dependencies(&1), [2]);
    }

    #[test]
    fn dependencies_keep_insertion_order() {
        let mut graph = Graph::new();
        graph.add_edge(1, 3);
        graph.add_edge(1, 2);
        assert_eq!(graph.dependencies(&1), [3, 2]);
    }

    #[test]
    fn unknown_node_has_no_dependencies() {
        let graph: Graph<u8> = Graph::new();
        assert!(graph.dependencies(&7).is_empty());
        assert!(graph.is_empty());
    }
}
