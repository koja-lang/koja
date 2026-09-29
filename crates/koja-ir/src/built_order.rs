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

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use koja_ast::ast::Diagnostic;
use koja_ast::span::Span;
use koja_typecheck::{GlobalKind, GlobalRegistry};

use crate::constant::IRConstantValue;
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

/// One `Built` constant the init of `from` reaches. `via` names the
/// function the read sits in when that function is not the init
/// itself, so the cycle diagnostic can name the call that closes the
/// loop.
struct Edge {
    to: IRSymbol,
    via: Option<IRSymbol>,
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
    let (functions, inits) = index(packages);
    let edges: BTreeMap<&IRSymbol, Vec<Edge>> = inits
        .iter()
        .map(|(constant, init)| (*constant, reaches(init, &inits, &functions)))
        .collect();

    let (order, stuck) = topological_order(&edges);
    if stuck.is_empty() {
        return Ok(order);
    }
    Err(stuck
        .iter()
        .map(|constant| diagnose_stuck(constant, &edges, &stuck, &span_of))
        .collect())
}

/// The `Built` constants the init function `init` reaches, for the
/// seal to check a stored order against.
pub(crate) fn reached_constants(init: &IRSymbol, packages: &[IRPackage]) -> Vec<IRSymbol> {
    let (functions, inits) = index(packages);
    reaches(init, &inits, &functions)
        .into_iter()
        .map(|edge| edge.to)
        .collect()
}

type FunctionIndex<'a> = BTreeMap<&'a str, &'a IRFunction>;
type InitIndex<'a> = BTreeMap<&'a IRSymbol, &'a IRSymbol>;

/// Index every function by mangled name and every `Built` constant
/// by its init symbol.
fn index(packages: &[IRPackage]) -> (FunctionIndex<'_>, InitIndex<'_>) {
    let functions = packages
        .iter()
        .flat_map(|package| package.functions.iter())
        .map(|(symbol, function)| (symbol.mangled(), function))
        .collect();
    let inits = packages
        .iter()
        .flat_map(|package| package.constants.iter())
        .filter_map(|(symbol, value)| match value {
            IRConstantValue::Built { init, .. } => Some((symbol, init)),
            _ => None,
        })
        .collect();
    (functions, inits)
}

/// Every `Built` constant loaded anywhere in the transitive call
/// graph of `init`. Intrinsics and externs have no blocks, so they
/// are leaves. A function the graph reaches twice is walked once.
fn reaches(init: &IRSymbol, inits: &InitIndex<'_>, functions: &FunctionIndex<'_>) -> Vec<Edge> {
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen_targets: BTreeSet<&IRSymbol> = BTreeSet::new();
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut queue: VecDeque<&IRSymbol> = VecDeque::from([init]);
    while let Some(symbol) = queue.pop_front() {
        if !visited.insert(symbol.mangled()) {
            continue;
        }
        let Some(function) = functions.get(symbol.mangled()) else {
            continue;
        };
        for block in &function.blocks {
            for instruction in &block.instructions {
                match instruction {
                    IRInstruction::Call { callee, .. } => queue.push_back(callee),
                    IRInstruction::MakeClosure { body, .. } => queue.push_back(body),
                    IRInstruction::LoadConst { const_id, .. } => {
                        if let Some((target, _)) = inits.get_key_value(const_id)
                            && seen_targets.insert(*target)
                        {
                            edges.push(Edge {
                                to: (*target).clone(),
                                via: (symbol != init).then(|| symbol.clone()),
                            });
                        }
                    }
                    _ => {}
                }
            }
            if let IRTerminator::TailCall { callee, .. } = &block.terminator {
                queue.push_back(callee);
            }
        }
    }
    edges
}

/// Kahn's algorithm seeded in symbol order, so the result is stable
/// across runs. Returns the ordered constants and the ones a cycle
/// left stuck, in symbol order.
fn topological_order(edges: &BTreeMap<&IRSymbol, Vec<Edge>>) -> (Vec<IRSymbol>, Vec<IRSymbol>) {
    let mut pending: BTreeMap<&IRSymbol, usize> = edges
        .iter()
        .map(|(constant, reads)| (*constant, reads.len()))
        .collect();
    let mut dependents: BTreeMap<&IRSymbol, Vec<&IRSymbol>> = BTreeMap::new();
    for (constant, reads) in edges {
        for edge in reads {
            dependents.entry(&edge.to).or_default().push(constant);
        }
    }

    let mut ready: VecDeque<&IRSymbol> = pending
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(constant, _)| *constant)
        .collect();
    let mut order = Vec::with_capacity(edges.len());
    while let Some(constant) = ready.pop_front() {
        order.push(constant.clone());
        for dependent in dependents.get(constant).into_iter().flatten() {
            let count = pending
                .get_mut(dependent)
                .expect("dependent is a Built constant");
            *count -= 1;
            if *count == 0 {
                ready.push_back(dependent);
            }
        }
    }

    let stuck = pending
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(constant, _)| (*constant).clone())
        .collect();
    (order, stuck)
}

/// The diagnostic for one constant a startup cycle left stuck. A
/// constant on the cycle names the hops that lead back to it, with
/// the function that closes each hop when the read sits in a callee.
/// A constant downstream of a cycle names the stuck constant it
/// waits on.
fn diagnose_stuck(
    constant: &IRSymbol,
    edges: &BTreeMap<&IRSymbol, Vec<Edge>>,
    stuck: &[IRSymbol],
    span_of: &impl Fn(&IRSymbol) -> Span,
) -> Diagnostic {
    let span = span_of(constant);
    if let Some(path) = cycle_from(constant, edges, stuck) {
        let hops: Vec<String> = path
            .iter()
            .flat_map(|edge| {
                let via = edge.via.as_ref().map(|function| format!("`{function}`"));
                let target = (edge.to != *constant).then(|| format!("`{}`", edge.to));
                via.into_iter().chain(target)
            })
            .collect();
        return Diagnostic::error(
            format!(
                "constant `{constant}` depends on itself at startup through {}",
                hops.join(", ")
            ),
            span,
        );
    }
    let blocker = edges
        .get(constant)
        .into_iter()
        .flatten()
        .map(|edge| &edge.to)
        .find(|target| stuck.contains(target))
        .expect("a stuck constant waits on another stuck constant");
    Diagnostic::error(
        format!("constant `{constant}` depends on `{blocker}`, which is in a startup cycle"),
        span,
    )
}

/// Depth-first search through the stuck constants for a path that
/// leads from `start` back to itself. Returns the edges along that
/// path, or `None` when `start` only waits on a cycle it is not part
/// of.
fn cycle_from<'a>(
    start: &IRSymbol,
    edges: &'a BTreeMap<&IRSymbol, Vec<Edge>>,
    stuck: &[IRSymbol],
) -> Option<Vec<&'a Edge>> {
    fn search<'a>(
        current: &IRSymbol,
        start: &IRSymbol,
        edges: &'a BTreeMap<&IRSymbol, Vec<Edge>>,
        stuck: &[IRSymbol],
        visited: &mut BTreeSet<IRSymbol>,
        path: &mut Vec<&'a Edge>,
    ) -> bool {
        for edge in edges.get(current).into_iter().flatten() {
            if !stuck.contains(&edge.to) {
                continue;
            }
            path.push(edge);
            if edge.to == *start {
                return true;
            }
            if visited.insert(edge.to.clone())
                && search(&edge.to, start, edges, stuck, visited, path)
            {
                return true;
            }
            path.pop();
        }
        false
    }

    let mut visited = BTreeSet::from([start.clone()]);
    let mut path = Vec::new();
    search(start, start, edges, stuck, &mut visited, &mut path).then_some(path)
}
