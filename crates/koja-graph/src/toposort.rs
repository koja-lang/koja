//! Kahn's algorithm with a stable seed.

use std::collections::{BTreeMap, VecDeque};

use crate::Graph;

/// The result of [`Graph::toposort`]. `ready` holds every node that
/// can be ordered, each after the nodes it depends on. `stuck` holds
/// every node on a dependency cycle or behind one, in key order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Order<K> {
    pub ready: Vec<K>,
    pub stuck: Vec<K>,
}

impl<K: Clone + Ord> Graph<K> {
    /// Order the nodes so every node follows its dependencies. Nodes
    /// with no pending dependencies are seeded in key order and
    /// released first in, first out, so independent nodes keep their
    /// key order. A node with a path back to itself never becomes
    /// ready, and neither does a node that depends on one.
    pub fn toposort(&self) -> Order<K> {
        let mut pending: BTreeMap<&K, usize> = self
            .nodes()
            .map(|node| (node, self.dependencies(node).len()))
            .collect();
        let mut dependents: BTreeMap<&K, Vec<&K>> = BTreeMap::new();
        for node in self.nodes() {
            for dependency in self.dependencies(node) {
                dependents.entry(dependency).or_default().push(node);
            }
        }

        let mut queue: VecDeque<&K> = pending
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(node, _)| *node)
            .collect();
        let mut ready = Vec::with_capacity(pending.len());
        while let Some(node) = queue.pop_front() {
            ready.push(node.clone());
            for dependent in dependents.get(node).into_iter().flatten() {
                let count = pending
                    .get_mut(dependent)
                    .expect("every dependent is a node of the graph");
                *count -= 1;
                if *count == 0 {
                    queue.push_back(dependent);
                }
            }
        }

        let stuck = pending
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(node, _)| (*node).clone())
            .collect();
        Order { ready, stuck }
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
    fn dependencies_come_first() {
        let order = graph(&[(1, 2), (2, 3)]).toposort();
        assert_eq!(order.ready, [3, 2, 1]);
        assert!(order.stuck.is_empty());
    }

    #[test]
    fn independent_nodes_keep_key_order() {
        let mut graph = graph(&[(3, 1)]);
        graph.add_node(2);
        graph.add_node(0);
        assert_eq!(graph.toposort().ready, [0, 1, 2, 3]);
    }

    #[test]
    fn self_dependency_is_stuck() {
        let order = graph(&[(1, 1)]).toposort();
        assert!(order.ready.is_empty());
        assert_eq!(order.stuck, [1]);
    }

    #[test]
    fn cycle_and_its_dependents_are_stuck() {
        let order = graph(&[(1, 2), (2, 1), (3, 1), (4, 5)]).toposort();
        assert_eq!(order.ready, [5, 4]);
        assert_eq!(order.stuck, [1, 2, 3]);
    }

    #[test]
    fn empty_graph_orders_nothing() {
        let order: Order<u8> = Graph::new().toposort();
        assert_eq!(
            order,
            Order {
                ready: Vec::new(),
                stuck: Vec::new(),
            }
        );
    }
}
