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

use std::collections::{BTreeSet, HashMap};

use koja_ast::ast::{
    Constant, Diagnostic, EnumConstructionData, Expr, ExprKind, FieldInit, Item, Name, name_texts,
    path_text,
};
use koja_ast::identifier::{GlobalRegistryId, Identifier, ResolvedType};
use koja_graph::Graph;

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

/// One constant waiting for its definition.
struct Node {
    id: GlobalRegistryId,
    position: Position,
}

/// Every constant waiting for its definition, and the reads between
/// them. A node is indexed by its position in `nodes`, and an edge
/// `a -> b` in `reads` means the value of `a` reads `b`.
struct Pending {
    nodes: Vec<Node>,
    reads: Graph<usize>,
}

/// Lift every constant in the program, each after the constants it
/// reads. Constants in or behind a dependency cycle are diagnosed and
/// stamped unresolved.
pub(super) fn lift_constants(
    packages: &mut [CheckedPackage],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let pending = collect_pending(packages, registry);
    let order = pending.reads.toposort();
    for index in order.ready {
        let node = &pending.nodes[index];
        lift_at(node.id, node.position, packages, registry, diagnostics);
    }
    diagnose_stuck(&pending, &order.stuck, packages, registry, diagnostics);
}

/// Every constant still waiting for its definition, with the reads
/// its value makes. Nodes are numbered in declaration order, so the
/// sort keeps independent constants in source order. A package that
/// declares one name twice yields one node, since collect diagnosed
/// the duplicate and both items map to the same registry entry.
fn collect_pending(packages: &[CheckedPackage], registry: &GlobalRegistry) -> Pending {
    let mut nodes = Vec::new();
    let mut index_of: HashMap<GlobalRegistryId, usize> = HashMap::new();
    let mut reads_by_node: Vec<Vec<GlobalRegistryId>> = Vec::new();
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
                if index_of.contains_key(&id) {
                    continue;
                }
                let mut reads = Vec::new();
                collect_reads(&constant.value, scope, &mut reads);
                index_of.insert(id, nodes.len());
                nodes.push(Node {
                    id,
                    position: Position {
                        file: file_index,
                        item: item_index,
                        package: package_index,
                    },
                });
                reads_by_node.push(reads);
            }
        }
    }

    let mut graph = Graph::new();
    for (index, reads) in reads_by_node.into_iter().enumerate() {
        graph.add_node(index);
        for read in reads.iter().filter_map(|id| index_of.get(id)) {
            graph.add_edge(index, *read);
        }
    }
    Pending {
        nodes,
        reads: graph,
    }
}

/// Lift the constant `id` at `position` with its file's scope.
fn lift_at(
    id: GlobalRegistryId,
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
    lift_constant(constant, id, &mut scope, diagnostics);
}

/// Diagnose every constant that could not lift and stamp it
/// unresolved. A constant on a cycle names the cycle. A constant
/// behind one names the stuck constant it reads.
fn diagnose_stuck(
    pending: &Pending,
    stuck: &[usize],
    packages: &[CheckedPackage],
    registry: &mut GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let stuck_set: BTreeSet<usize> = stuck.iter().copied().collect();
    let name_of =
        |index: usize| path_text(&constant_at(pending.nodes[index].position, packages).path);
    for &index in stuck {
        let constant = constant_at(pending.nodes[index].position, packages);
        let name = path_text(&constant.path);
        let message = match pending.reads.cycle_path(&index, &stuck_set) {
            Some(cycle) if cycle.is_empty() => format!("constant `{name}` depends on itself"),
            Some(cycle) => {
                let through: Vec<String> = cycle
                    .iter()
                    .map(|&other| format!("`{}`", name_of(other)))
                    .collect();
                format!(
                    "constant `{name}` depends on itself through {}",
                    through.join(", ")
                )
            }
            None => {
                let read = pending
                    .reads
                    .dependencies(&index)
                    .iter()
                    .find(|read| stuck_set.contains(read))
                    .expect("a stuck node outside every cycle reads a stuck node");
                format!(
                    "constant `{name}` depends on `{}`, which is in a dependency cycle",
                    name_of(*read)
                )
            }
        };
        diagnostics.push(Diagnostic::error(message, constant.span));
        registry.set_constant_definition(
            pending.nodes[index].id,
            ConstantDefinition {
                ty: ResolvedType::unresolved(),
                value: constant.value.clone(),
            },
        );
    }
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
