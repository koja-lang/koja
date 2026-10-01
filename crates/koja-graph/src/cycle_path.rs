//! Depth-first search for a dependency path that returns to its start.

use std::collections::BTreeSet;

use crate::Graph;

impl<K: Clone + Ord> Graph<K> {
    /// The nodes on a path of dependencies that leads from `start`
    /// back to `start`, in the order the path visits them, with
    /// `start` itself left out. Only nodes in `within` are followed.
    /// A caller passes the `stuck` set from [`Graph::toposort`] so
    /// the search stays off nodes that are already ordered.
    ///
    /// `Some` of an empty path is a node that depends on itself.
    /// `None` is a node that only depends on a cycle it is not part
    /// of.
    pub fn cycle_path(&self, start: &K, within: &BTreeSet<K>) -> Option<Vec<K>> {
        let mut search = Search {
            graph: self,
            path: Vec::new(),
            start,
            visited: BTreeSet::from([start.clone()]),
            within,
        };
        search.walk(start).then_some(search.path)
    }
}

struct Search<'a, K> {
    graph: &'a Graph<K>,
    path: Vec<K>,
    start: &'a K,
    visited: BTreeSet<K>,
    within: &'a BTreeSet<K>,
}

impl<K: Clone + Ord> Search<'_, K> {
    fn walk(&mut self, current: &K) -> bool {
        let graph = self.graph;
        for dependency in graph.dependencies(current) {
            if dependency == self.start {
                return true;
            }
            if !self.within.contains(dependency) || !self.visited.insert(dependency.clone()) {
                continue;
            }
            self.path.push(dependency.clone());
            if self.walk(dependency) {
                return true;
            }
            self.path.pop();
        }
        false
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
    fn self_dependency_is_an_empty_path() {
        let graph = graph(&[(1, 1)]);
        assert_eq!(graph.cycle_path(&1, &BTreeSet::from([1])), Some(Vec::new()));
    }

    #[test]
    fn path_lists_the_other_nodes_in_visit_order() {
        let graph = graph(&[(1, 2), (2, 3), (3, 1)]);
        let stuck = BTreeSet::from([1, 2, 3]);
        assert_eq!(graph.cycle_path(&1, &stuck), Some(vec![2, 3]));
        assert_eq!(graph.cycle_path(&2, &stuck), Some(vec![3, 1]));
    }

    #[test]
    fn node_behind_a_cycle_has_no_path() {
        let graph = graph(&[(1, 2), (2, 1), (3, 1)]);
        let stuck = BTreeSet::from([1, 2, 3]);
        assert_eq!(graph.cycle_path(&3, &stuck), None);
    }

    #[test]
    fn search_stays_within_the_given_set() {
        // 1 -> 4 -> 1 is a cycle, but 4 is outside `within`, so the
        // search cannot use it to return.
        let graph = graph(&[(1, 4), (4, 1)]);
        assert_eq!(graph.cycle_path(&1, &BTreeSet::from([1])), None);
    }

    #[test]
    fn dead_ends_are_backtracked() {
        let graph = graph(&[(1, 2), (1, 3), (3, 1)]);
        let stuck = BTreeSet::from([1, 2, 3]);
        assert_eq!(graph.cycle_path(&1, &stuck), Some(vec![3]));
    }
}
