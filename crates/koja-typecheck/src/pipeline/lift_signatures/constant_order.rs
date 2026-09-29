//! Constant lift in dependency order.
//!
//! A constant value may read other constants, bare (`MAX`), through
//! an owner (`Duration.ZERO`), or through a package (`Mathlib.PI`),
//! and the resolver needs each one stamped before it reads it. Source
//! order is not a rule for package-level items, so this pass finds
//! the reads first, orders the constants so every read comes after
//! its target, and lifts them in that order. A read inside an omitted
//! field's default counts too, because resolution fills the field
//! from the stored default in the declaring scope.
//!
//! A cycle has no value. Every constant in one, and every constant
//! that depends on one, gets a diagnostic and an unresolved
//! definition, so readers see an unresolved type instead of a missing
//! definition.

use std::collections::{HashMap, HashSet, VecDeque};

use koja_ast::ast::{
    Constant, Diagnostic, EnumConstructionData, Expr, ExprKind, FieldInit, Item, Name, name_texts,
    path_text,
};
use koja_ast::identifier::{GlobalRegistryId, Identifier, ResolvedType};

use crate::pipeline::aliases::collect_file_aliases;
use crate::pipeline::resolve::types::lookup_type;
use crate::pipeline::resolve::{
    constant_named_by_ident, constant_named_by_path, declaring_scope, static_dotted_path,
};
use crate::program::CheckedPackage;
use crate::registry::{
    ConstantDefinition, GlobalKind, GlobalRegistry, ResolvedStructField, ResolvedVariantData,
};

use super::LiftScope;
use super::constants::lift_constant;
use super::types::ResolutionScope;

/// Where a constant item sits in the program.
#[derive(Clone, Copy)]
struct Position {
    file: usize,
    item: usize,
    package: usize,
}

/// One constant in the dependency graph. `reads` holds the indices
/// of the constants its value reads, duplicates included.
struct Node {
    id: GlobalRegistryId,
    position: Position,
    reads: Vec<usize>,
}

/// Lift every constant in the program, each after the constants it
/// reads. Constants in or behind a dependency cycle are diagnosed and
/// stamped unresolved.
pub(super) fn lift_constants(
    packages: &mut [CheckedPackage],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let nodes = collect_nodes(packages, registry);
    let (ready, stuck) = topological_order(&nodes);
    for index in ready {
        lift_at(nodes[index].position, packages, registry, diagnostics);
    }
    diagnose_stuck(&nodes, &stuck, packages, registry, diagnostics);
}

/// Every constant still waiting for its definition, with the reads
/// its value makes.
fn collect_nodes(packages: &[CheckedPackage], registry: &GlobalRegistry) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut reads_by_node = Vec::new();
    for (package_index, package) in packages.iter().enumerate() {
        for (file_index, file) in package.files.iter().enumerate() {
            let aliases = collect_file_aliases(file);
            let scope = ResolutionScope {
                aliases: &aliases,
                package: &package.package,
                registry,
            };
            for (item_index, item) in file.items.iter().enumerate() {
                let Item::Constant(constant) = item else {
                    continue;
                };
                let identifier = Identifier::new(&package.package, name_texts(&constant.path));
                let Some((id, entry)) = registry.lookup(&identifier) else {
                    panic!(
                        "lift_signatures found constant `{identifier}` missing from registry. \
                         This is a collect invariant violation",
                    );
                };
                // The name belongs to another declaration that
                // registered first. Collect diagnosed the collision.
                if !matches!(entry.kind, GlobalKind::Constant(None)) {
                    continue;
                }
                let mut reads = Vec::new();
                collect_reads(&constant.value, scope, &mut reads);
                nodes.push(Node {
                    id,
                    position: Position {
                        file: file_index,
                        item: item_index,
                        package: package_index,
                    },
                    reads: Vec::new(),
                });
                reads_by_node.push(reads);
            }
        }
    }
    let index_of: HashMap<GlobalRegistryId, usize> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect();
    for (node, reads) in nodes.iter_mut().zip(reads_by_node) {
        node.reads = reads
            .into_iter()
            .filter_map(|id| index_of.get(&id).copied())
            .collect();
    }
    nodes
}

/// Kahn's algorithm over `reads`. Returns the nodes that can lift, in
/// an order where every node follows the nodes it reads, and the
/// nodes that cannot, because they sit in or behind a cycle. Seeds
/// in declaration order so independent constants keep their source
/// order.
fn topological_order(nodes: &[Node]) -> (Vec<usize>, Vec<usize>) {
    let mut pending: Vec<usize> = nodes.iter().map(|node| node.reads.len()).collect();
    let mut readers: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        for &read in &node.reads {
            readers[read].push(index);
        }
    }
    let mut queue: VecDeque<usize> = (0..nodes.len()).filter(|&i| pending[i] == 0).collect();
    let mut ready = Vec::with_capacity(nodes.len());
    while let Some(index) = queue.pop_front() {
        ready.push(index);
        for &reader in &readers[index] {
            pending[reader] -= 1;
            if pending[reader] == 0 {
                queue.push_back(reader);
            }
        }
    }
    let stuck = (0..nodes.len()).filter(|&i| pending[i] > 0).collect();
    (ready, stuck)
}

/// Lift the constant at `position` with its file's scope.
fn lift_at(
    position: Position,
    packages: &mut [CheckedPackage],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let package = &mut packages[position.package];
    let file = &mut package.files[position.file];
    let aliases = collect_file_aliases(file);
    let mut scope = LiftScope {
        aliases: &aliases,
        package: &package.package,
        registry,
    };
    let Item::Constant(constant) = &mut file.items[position.item] else {
        unreachable!("positions come from `Item::Constant` items");
    };
    lift_constant(constant, &mut scope, diagnostics);
}

/// Diagnose every constant that could not lift and stamp it
/// unresolved. A constant on a cycle names the cycle. A constant
/// behind one names the stuck constant it reads.
fn diagnose_stuck(
    nodes: &[Node],
    stuck: &[usize],
    packages: &[CheckedPackage],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let stuck_set: HashSet<usize> = stuck.iter().copied().collect();
    for &index in stuck {
        let constant = constant_at(nodes[index].position, packages);
        let name = path_text(&constant.path);
        let message = match cycle_from(index, nodes, &stuck_set) {
            Some(cycle) if cycle.is_empty() => format!("constant `{name}` depends on itself"),
            Some(cycle) => {
                let through: Vec<String> = cycle
                    .iter()
                    .map(|&other| {
                        format!(
                            "`{}`",
                            path_text(&constant_at(nodes[other].position, packages).path)
                        )
                    })
                    .collect();
                format!(
                    "constant `{name}` depends on itself through {}",
                    through.join(", ")
                )
            }
            None => {
                let read = nodes[index]
                    .reads
                    .iter()
                    .find(|read| stuck_set.contains(read))
                    .expect("a stuck node outside every cycle reads a stuck node");
                format!(
                    "constant `{name}` depends on `{}`, which is in a dependency cycle",
                    path_text(&constant_at(nodes[*read].position, packages).path)
                )
            }
        };
        diagnostics.push(Diagnostic::error(message, constant.span));
        registry.set_constant_definition(
            nodes[index].id,
            ConstantDefinition {
                ty: ResolvedType::unresolved(),
                value: constant.value.clone(),
            },
        );
    }
}

/// The other nodes on a cycle through `start`, in read order, or
/// `None` when no path of stuck reads returns to `start`. An empty
/// cycle is a constant that reads itself.
fn cycle_from(start: usize, nodes: &[Node], stuck: &HashSet<usize>) -> Option<Vec<usize>> {
    fn walk(
        current: usize,
        start: usize,
        nodes: &[Node],
        stuck: &HashSet<usize>,
        path: &mut Vec<usize>,
        visited: &mut HashSet<usize>,
    ) -> bool {
        for &read in &nodes[current].reads {
            if read == start {
                return true;
            }
            if !stuck.contains(&read) || !visited.insert(read) {
                continue;
            }
            path.push(read);
            if walk(read, start, nodes, stuck, path, visited) {
                return true;
            }
            path.pop();
        }
        false
    }

    let mut path = Vec::new();
    let mut visited = HashSet::from([start]);
    walk(start, start, nodes, stuck, &mut path, &mut visited).then_some(path)
}

fn constant_at(position: Position, packages: &[CheckedPackage]) -> &Constant {
    let Item::Constant(constant) =
        &packages[position.package].files[position.file].items[position.item]
    else {
        unreachable!("positions come from `Item::Constant` items");
    };
    constant
}

/// Every constant `expr` reads, in `scope`, including reads inside
/// the defaults of fields a struct literal omits. Names that turn out
/// not to be constants are left for the resolver to diagnose.
fn collect_reads(expr: &Expr, scope: ResolutionScope<'_>, reads: &mut Vec<GlobalRegistryId>) {
    match &expr.kind {
        ExprKind::BinaryLiteral { segments } => {
            for segment in segments {
                collect_reads(&segment.value, scope, reads);
            }
        }
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => match data {
            EnumConstructionData::Struct(fields) => {
                collect_field_reads(fields, scope, reads);
                if let Some((owner_id, declared)) = struct_shaped_fields(type_path, variant, scope)
                {
                    collect_default_reads(owner_id, declared, fields, scope.registry, reads);
                }
            }
            EnumConstructionData::Tuple(elements) => {
                for element in elements {
                    collect_reads(element, scope, reads);
                }
            }
            EnumConstructionData::Unit => {
                if let Some(path) = static_dotted_path(&expr.kind) {
                    reads.extend(constant_named_by_path(&path, scope));
                }
            }
        },
        kind @ ExprKind::FieldAccess { .. } => {
            if let Some(path) = static_dotted_path(kind) {
                reads.extend(constant_named_by_path(&path, scope));
            }
        }
        ExprKind::Group { expr: inner } | ExprKind::Unary { operand: inner, .. } => {
            collect_reads(inner, scope, reads);
        }
        ExprKind::Ident { name, .. } => reads.extend(constant_named_by_ident(name, scope)),
        ExprKind::List { elements } => {
            for element in elements {
                collect_reads(element, scope, reads);
            }
        }
        ExprKind::Map { entries } => {
            for (key, value) in entries {
                collect_reads(key, scope, reads);
                collect_reads(value, scope, reads);
            }
        }
        ExprKind::StructConstruction { type_path, fields } => {
            collect_field_reads(fields, scope, reads);
            if let Some((struct_id, entry)) = lookup_type(&name_texts(type_path), scope)
                && let GlobalKind::Struct(Some(definition)) = &entry.kind
            {
                collect_default_reads(struct_id, &definition.fields, fields, scope.registry, reads);
            }
        }
        _ => {}
    }
}

fn collect_field_reads(
    fields: &[FieldInit],
    scope: ResolutionScope<'_>,
    reads: &mut Vec<GlobalRegistryId>,
) {
    for field in fields {
        collect_reads(&field.value, scope, reads);
    }
}

/// Reads inside the defaults of the `declared` fields that `inits`
/// omits. A default resolves in its owner's declaring file, so the
/// scan uses that scope rather than the literal's.
fn collect_default_reads(
    owner_id: GlobalRegistryId,
    declared: &[ResolvedStructField],
    inits: &[FieldInit],
    registry: &GlobalRegistry,
    reads: &mut Vec<GlobalRegistryId>,
) {
    let (package, aliases) = declaring_scope(owner_id, registry);
    let scope = ResolutionScope {
        aliases,
        package,
        registry,
    };
    for field in declared {
        if inits.iter().any(|init| init.name.text == field.name) {
            continue;
        }
        if let Some(default) = &field.default {
            collect_reads(default, scope, reads);
        }
    }
}

/// The owner and declared fields behind a struct-shaped
/// `EnumConstruction`, which is either a dotted struct literal such
/// as `Pkg.Type{...}` or a struct variant such as `Shape.Rect{...}`.
fn struct_shaped_fields<'r>(
    type_path: &[Name],
    variant: &Name,
    scope: ResolutionScope<'r>,
) -> Option<(GlobalRegistryId, &'r [ResolvedStructField])> {
    let mut full_path = name_texts(type_path);
    full_path.push(variant.text.clone());
    if let Some((struct_id, entry)) = lookup_type(&full_path, scope)
        && let GlobalKind::Struct(Some(definition)) = &entry.kind
    {
        return Some((struct_id, &definition.fields));
    }
    let (enum_id, entry) = lookup_type(&name_texts(type_path), scope)?;
    let GlobalKind::Enum(Some(definition)) = &entry.kind else {
        return None;
    };
    let (_, resolved) = definition.lookup_variant(variant.as_str())?;
    match &resolved.data {
        ResolvedVariantData::Struct(fields) => Some((enum_id, fields)),
        _ => None,
    }
}
