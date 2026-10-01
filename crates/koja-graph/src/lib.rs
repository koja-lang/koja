//! Algorithms over a directed graph with ordered keys.
//!
//! The compiler orders constants, enum layouts, and init functions by
//! the edges between them. Each pass used to carry its own sort and
//! its own cycle walk. This crate holds the shared versions. Nothing
//! here knows a symbol from a path. A node is any `K: Clone + Ord`,
//! and the caller maps the result back to its own vocabulary.
//!
//! An edge `from -> to` reads as "`from` depends on `to`". Every
//! algorithm visits nodes in key order and the edges of one node in
//! insertion order, so results are stable across runs.

mod back_edges;
mod cycle_path;
mod graph;
mod reachable;
mod toposort;

pub use graph::Graph;
pub use toposort::Order;
