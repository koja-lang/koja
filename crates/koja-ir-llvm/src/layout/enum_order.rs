//! Topological dependency order for the enum-body define phase.
//!
//! [`super::enums::define_enum_completes_and_outer`] queries
//! `get_abi_size` and `get_abi_alignment` on each variant's
//! complete struct, and `get_abi_size` returns 0 (and alignment
//! returns 1) when the queried struct transitively references an
//! opaque named type. For enum-on-enum dependencies that means an
//! enum E whose payload references enum F's outer chunk has to
//! wait until F's outer body is set, otherwise E's outer would
//! collapse to a 1-byte chunk.
//!
//! [`enums_in_dependency_order`] returns every program enum decl
//! in an order where every dependency lands before its dependants.
//! Struct field types are followed transitively so a payload like
//! `Wrapper { inner: TokenKind }` still threads `TokenKind`'s outer
//! into the dependency set. Enums with no dependency between them
//! keep symbol order.
//!
//! Unresolved references (symbols missing from the program) are
//! skipped rather than treated as errors. They contribute no size
//! dependency this walk can honor.
//!
//! Pure IR-data walk over a [`koja_graph::Graph`], no LLVM types
//! touched here.

use std::collections::{BTreeMap, BTreeSet};

use koja_graph::Graph;
use koja_ir::{IREnumDecl, IRPackage, IRStructField, IRSymbol, IRType, IRVariantPayload};

/// Topologically sort every enum decl across `packages` so an enum
/// whose payload references another enum lands after it. See the
/// module doc for why. Panics on a dependency cycle, which
/// `koja_ir::cycle` breaks with `Indirect` before emit.
pub(crate) fn enums_in_dependency_order(packages: &[IRPackage]) -> Vec<&IREnumDecl> {
    let enum_index = build_enum_index(packages);
    let struct_field_index = build_struct_field_index(packages);
    let mut graph: Graph<&IRSymbol> = Graph::new();
    for (symbol, decl) in &enum_index {
        graph.add_node(symbol);
        for dependency in enum_dependencies(decl, &struct_field_index) {
            if let Some((target, _)) = enum_index.get_key_value(&dependency) {
                graph.add_edge(symbol, target);
            }
        }
    }
    let order = graph.toposort();
    if let Some(stuck) = order.stuck.first() {
        panic!(
            "LLVM emit: enum `{stuck}` sits on a payload dependency cycle that \
             `koja_ir::cycle` should have broken",
        );
    }
    order
        .ready
        .into_iter()
        .map(|symbol| enum_index[symbol])
        .collect()
}

fn build_enum_index(packages: &[IRPackage]) -> BTreeMap<IRSymbol, &IREnumDecl> {
    let mut map = BTreeMap::new();
    for package in packages {
        for decl in package.enums.values() {
            map.insert(decl.symbol.clone(), decl);
        }
    }
    map
}

fn build_struct_field_index(packages: &[IRPackage]) -> BTreeMap<IRSymbol, &[IRStructField]> {
    let mut map = BTreeMap::new();
    for package in packages {
        for decl in package.structs.values() {
            map.insert(decl.symbol.clone(), decl.fields.as_slice());
        }
    }
    map
}

/// Every enum symbol that `decl`'s payloads reference inline, in
/// symbol order.
fn enum_dependencies(
    decl: &IREnumDecl,
    struct_field_index: &BTreeMap<IRSymbol, &[IRStructField]>,
) -> BTreeSet<IRSymbol> {
    let mut deps: BTreeSet<IRSymbol> = BTreeSet::new();
    for variant in &decl.variants {
        collect_payload_enum_refs(&variant.payload, struct_field_index, &mut deps);
    }
    deps
}

fn collect_payload_enum_refs(
    payload: &IRVariantPayload,
    struct_field_index: &BTreeMap<IRSymbol, &[IRStructField]>,
    deps: &mut BTreeSet<IRSymbol>,
) {
    match payload {
        IRVariantPayload::Struct(fields) => {
            for field in fields {
                collect_type_enum_refs(&field.ir_type, struct_field_index, deps);
            }
        }
        IRVariantPayload::Tuple(types) => {
            for ty in types {
                collect_type_enum_refs(ty, struct_field_index, deps);
            }
        }
        IRVariantPayload::Unit => {}
    }
}

fn collect_type_enum_refs(
    ty: &IRType,
    struct_field_index: &BTreeMap<IRSymbol, &[IRStructField]>,
    deps: &mut BTreeSet<IRSymbol>,
) {
    match ty {
        IRType::Enum(symbol) => {
            deps.insert(symbol.clone());
        }
        IRType::Struct(symbol) => {
            // The struct's body is already set, but its size still
            // depends on any inner enum being bodied.
            if let Some(fields) = struct_field_index.get(symbol) {
                for field in *fields {
                    collect_type_enum_refs(&field.ir_type, struct_field_index, deps);
                }
            }
        }
        IRType::Tuple(elements) => {
            for element in elements {
                collect_type_enum_refs(element, struct_field_index, deps);
            }
        }
        IRType::Union { members, .. } => {
            for member in members {
                collect_type_enum_refs(member, struct_field_index, deps);
            }
        }
        // For heap-pointer payloads, the inner type lives behind a
        // pointer and contributes no inline size dependency to the
        // outer enum chunk computation. `Indirect` is the cycle-
        // breaking pointer minted by `koja_ir::cycle`.
        IRType::CPtr(_)
        | IRType::Function { .. }
        | IRType::Indirect(_)
        | IRType::List(_)
        | IRType::Map { .. }
        | IRType::Set(_) => {}
        // Primitive leaves carry no enum references.
        IRType::Binary
        | IRType::Bits
        | IRType::Bool
        | IRType::Float32
        | IRType::Float64
        | IRType::Int8
        | IRType::Int16
        | IRType::Int32
        | IRType::Int64
        | IRType::String
        | IRType::UInt8
        | IRType::UInt16
        | IRType::UInt32
        | IRType::UInt64
        | IRType::Unit => {}
    }
}
