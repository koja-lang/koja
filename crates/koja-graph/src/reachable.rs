//! Breadth-first reachability.

use std::collections::{BTreeSet, VecDeque};

use crate::Graph;

impl<K: Clone + Ord> Graph<K> {
    /// Every node reachable from `start` by following dependencies,
    /// in breadth-first order with `start` first. Each node appears
    /// once. A `start` the graph does not hold is returned alone.
    pub fn reachable(&self, start: &K) -> Vec<K> {
        let mut seen = BTreeSet::from([start.clone()]);
        let mut queue = VecDeque::from([start.clone()]);
        let mut order = Vec::new();
        while let Some(node) = queue.pop_front() {
            for dependency in self.dependencies(&node) {
                if seen.insert(dependency.clone()) {
                    queue.push_back(dependency.clone());
                }
            }
            order.push(node);
        }
        order
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
    fn visits_breadth_first_from_start() {
        let graph = graph(&[(1, 2), (1, 3), (2, 4), (3, 4), (4, 5)]);
        assert_eq!(graph.reachable(&1), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn cycles_terminate() {
        let graph = graph(&[(1, 2), (2, 1)]);
        assert_eq!(graph.reachable(&1), [1, 2]);
    }

    #[test]
    fn unrelated_nodes_are_left_out() {
        let graph = graph(&[(1, 2), (3, 4)]);
        assert_eq!(graph.reachable(&3), [3, 4]);
    }

    #[test]
    fn unknown_start_is_returned_alone() {
        let graph: Graph<u8> = Graph::new();
        assert_eq!(graph.reachable(&9), [9]);
    }
}
