//! Startup order for [`IRConstantValue::Built`] constants.
//!
//! Typecheck orders constants by the reads it can see in their
//! values. A `Built` init can also reach a constant through a call,
//! for example a carrier's `from_list` body that reads another
//! constant. Only the IR sees that edge, after monomorphization, so
//! this pass walks the transitive call graph of every init and sorts
//! the `Built` entries so each one runs after the constants its init
//! reaches. A cycle here is the one shape typecheck cannot catch. It
//! becomes a lowering diagnostic instead of an infinite recursion in
//! eval or a zeroed global in LLVM.

use std::collections::{BTreeMap, BTreeSet};

use koja_ast::ast::Diagnostic;
use koja_ast::span::Span;
use koja_graph::Graph;
use koja_typecheck::{GlobalKind, GlobalRegistry};

use crate::constant::IRConstantValue;
use crate::declarations::Declarations;
use crate::function::{IRFunction, IRInstruction, IRSymbol, IRTerminator};
use crate::package::IRPackage;

/// The declaration span of the constant behind `symbol`, for the
/// startup cycle diagnostic. The pool keys constants by the symbol
/// minted from their registry identifier, so the registry entry
/// whose identifier mints the same symbol is the declaration. Falls
/// back to the placeholder span when no entry matches.
pub(crate) fn constant_span(symbol: &IRSymbol, registry: &GlobalRegistry) -> Span {
    registry
        .iter()
        .filter(|(_, entry)| matches!(entry.kind, GlobalKind::Constant(_)))
        .find(|(_, entry)| IRSymbol::from_identifier(&entry.identifier) == *symbol)
        .map_or_else(Span::zero, |(_, entry)| entry.span)
}

/// One `Built` constant an init reaches. `via` names the function
/// the read sits in when that function is not the init itself, so
/// the cycle diagnostic can name the call that closes the loop.
struct Edge<'a> {
    to: &'a IRSymbol,
    via: Option<&'a IRSymbol>,
}

/// Compute the order backends run `Built` inits in. Every `Built`
/// entry appears exactly once, after every entry its init reaches.
/// `span_of` maps a constant's symbol back to its declaration so a
/// cycle diagnostic points at source. When a startup cycle exists,
/// the result carries one diagnostic per constant stuck behind it.
pub(crate) fn built_constant_order(
    packages: &[IRPackage],
    span_of: impl Fn(&IRSymbol) -> Span,
) -> Result<Vec<IRSymbol>, Vec<Diagnostic>> {
    let dependencies = Dependencies::new(&InitGraph::new(&Declarations::new(packages)));
    let order = dependencies.graph.toposort();
    if order.stuck.is_empty() {
        return Ok(order.ready.into_iter().cloned().collect());
    }
    let stuck: BTreeSet<&IRSymbol> = order.stuck.iter().copied().collect();
    Err(stuck
        .iter()
        .map(|constant| diagnose_stuck(constant, &dependencies, &stuck, &span_of))
        .collect())
}

/// The call graph of every function in `packages`, with the `Built`
/// constants each function loads. Built once per lowering entry and
/// once per seal, then queried per init.
pub(crate) struct InitGraph<'a> {
    calls: Graph<&'a IRSymbol>,
    inits: BTreeMap<&'a IRSymbol, &'a IRSymbol>,
    loads: BTreeMap<&'a IRSymbol, Vec<&'a IRSymbol>>,
}

impl<'a> InitGraph<'a> {
    pub(crate) fn new(declarations: &Declarations<'a>) -> Self {
        let inits = declarations
            .constants()
            .filter_map(|(symbol, value)| match value {
                IRConstantValue::Built { init, .. } => Some((symbol, init)),
                _ => None,
            })
            .collect();
        let mut graph = Self {
            calls: Graph::new(),
            inits,
            loads: BTreeMap::new(),
        };
        for function in declarations.functions() {
            graph.index_function(function);
        }
        graph
    }

    /// Record the callees of `function` and the `Built` constants it
    /// loads. Intrinsics and externs have no blocks, so they are
    /// leaves of the call graph.
    fn index_function(&mut self, function: &'a IRFunction) {
        let caller = &function.symbol;
        self.calls.add_node(caller);
        for block in &function.blocks {
            for instruction in &block.instructions {
                match instruction {
                    IRInstruction::Call { callee, .. } => self.calls.add_edge(caller, callee),
                    IRInstruction::LoadConst { const_id, .. } => {
                        if let Some((constant, _)) = self.inits.get_key_value(&const_id) {
                            self.loads.entry(caller).or_default().push(constant);
                        }
                    }
                    IRInstruction::MakeClosure { body, .. } => self.calls.add_edge(caller, body),
                    _ => {}
                }
            }
            if let IRTerminator::TailCall { callee, .. } = &block.terminator {
                self.calls.add_edge(caller, callee);
            }
        }
    }

    /// Every `Built` constant loaded anywhere in the transitive call
    /// graph of `init`, each once, with the function its first read
    /// sits in.
    fn reaches(&self, init: &'a IRSymbol) -> Vec<Edge<'a>> {
        let mut seen = BTreeSet::new();
        let mut edges = Vec::new();
        for function in self.calls.reachable(&init) {
            for constant in self.loads.get(&function).into_iter().flatten() {
                if seen.insert(*constant) {
                    edges.push(Edge {
                        to: constant,
                        via: (function != init).then_some(function),
                    });
                }
            }
        }
        edges
    }

    /// The `Built` constants the init function `init` reaches, for
    /// the seal to check a stored order against.
    pub(crate) fn reached_constants(&self, init: &'a IRSymbol) -> Vec<&'a IRSymbol> {
        self.reaches(init).into_iter().map(|edge| edge.to).collect()
    }
}

/// The dependency edges between `Built` constants, with the function
/// that closes an edge when the read sits in a callee of the init.
struct Dependencies<'a> {
    graph: Graph<&'a IRSymbol>,
    via: BTreeMap<(&'a IRSymbol, &'a IRSymbol), &'a IRSymbol>,
}

impl<'a> Dependencies<'a> {
    fn new(inits: &InitGraph<'a>) -> Self {
        let mut dependencies = Self {
            graph: Graph::new(),
            via: BTreeMap::new(),
        };
        for (constant, init) in &inits.inits {
            dependencies.graph.add_node(constant);
            for edge in inits.reaches(init) {
                dependencies.graph.add_edge(constant, edge.to);
                if let Some(function) = edge.via {
                    dependencies.via.insert((constant, edge.to), function);
                }
            }
        }
        dependencies
    }

    /// The hops a cycle diagnostic names, following `path` from
    /// `constant` back to itself. Each hop names the function that
    /// closes it, when the read sits in a callee, then the constant
    /// it lands on, except the final return to `constant`.
    fn hops(&self, constant: &'a IRSymbol, path: &[&'a IRSymbol]) -> Vec<String> {
        let mut hops = Vec::new();
        let mut from = constant;
        for &to in path.iter().chain([&constant]) {
            if let Some(function) = self.via.get(&(from, to)) {
                hops.push(format!("`{function}`"));
            }
            if to != constant {
                hops.push(format!("`{to}`"));
            }
            from = to;
        }
        hops
    }
}

/// The diagnostic for one constant a startup cycle left stuck. A
/// constant on the cycle names the hops that lead back to it, with
/// the function that closes each hop when the read sits in a callee.
/// A constant downstream of a cycle names the stuck constant it
/// waits on.
fn diagnose_stuck<'a>(
    constant: &'a IRSymbol,
    dependencies: &Dependencies<'a>,
    stuck: &BTreeSet<&'a IRSymbol>,
    span_of: &impl Fn(&IRSymbol) -> Span,
) -> Diagnostic {
    let span = span_of(constant);
    if let Some(path) = dependencies.graph.cycle_path(&constant, stuck) {
        return Diagnostic::error(
            format!(
                "constant `{constant}` depends on itself at startup through {}",
                dependencies.hops(constant, &path).join(", ")
            ),
            span,
        );
    }
    let blocker = dependencies
        .graph
        .dependencies(&constant)
        .iter()
        .find(|target| stuck.contains(*target))
        .expect("a stuck constant waits on another stuck constant");
    Diagnostic::error(
        format!("constant `{constant}` depends on `{blocker}`, which is in a startup cycle"),
        span,
    )
}
