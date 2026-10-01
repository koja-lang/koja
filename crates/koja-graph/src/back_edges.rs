//! Depth-first search for the edges that close a cycle.

use std::collections::BTreeSet;

use crate::Graph;

impl<K: Clone + Ord> Graph<K> {
    /// Every edge `(from, to)` whose target is still on the search
    /// stack when the depth-first walk reaches it. These edges close
    /// the cycles of the graph, and cutting each of them leaves the
    /// graph acyclic. The walk starts at each node in key order and
    /// follows the edges of one node in insertion order, so the
    /// result is stable across runs. A self loop is a back edge.
    pub fn back_edges(&self) -> Vec<(K, K)> {
        let mut search = Search {
            finished: BTreeSet::new(),
            found: Vec::new(),
            graph: self,
            on_stack: BTreeSet::new(),
        };
        for node in self.nodes() {
            if !search.finished.contains(node) {
                search.walk(node);
            }
        }
        search.found
    }
}

struct Search<'a, K> {
    finished: BTreeSet<K>,
    found: Vec<(K, K)>,
    graph: &'a Graph<K>,
    on_stack: BTreeSet<K>,
}

impl<K: Clone + Ord> Search<'_, K> {
    fn walk(&mut self, node: &K) {
        self.on_stack.insert(node.clone());
        for dependency in self.graph.dependencies(node) {
            if self.on_stack.contains(dependency) {
                self.found.push((node.clone(), dependency.clone()));
            } else if !self.finished.contains(dependency) {
                self.walk(dependency);
            }
        }
        self.on_stack.remove(node);
        self.finished.insert(node.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(edges: &[(u8, u8)]) -> Graph<u8> {
        let mut graph = Graph::new();
        for &(from, to) in edges {
            graph.add_edge(from, to);
        }
        graph
    }

    #[test]
    fn self_loop_is_a_back_edge() {
        let graph = graph(&[(1, 1)]);
        assert_eq!(graph.back_edges(), [(1, 1)]);
    }

    #[test]
    fn two_cycle_reports_the_closing_edge() {
        // The walk starts at 1, so 2 -> 1 is the edge that returns.
        let graph = graph(&[(1, 2), (2, 1)]);
        assert_eq!(graph.back_edges(), [(2, 1)]);
    }

    #[test]
    fn diamond_has_no_back_edges() {
        // 4 is finished by the time 3 reaches it, so a second path
        // to a node is not a cycle.
        let graph = graph(&[(1, 2), (1, 3), (2, 4), (3, 4)]);
        assert!(graph.back_edges().is_empty());
    }

    #[test]
    fn each_cycle_reports_one_edge() {
        let graph = graph(&[(1, 2), (2, 1), (3, 4), (4, 3)]);
        assert_eq!(graph.back_edges(), [(2, 1), (4, 3)]);
    }
}
