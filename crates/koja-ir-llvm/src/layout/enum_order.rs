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
//! into the dependency set.
//!
//! Pure IR-data walk, no LLVM types touched here. Lives next to
//! the body-define modules so the order constraint and the consumer
//! sit side-by-side.

use std::collections::{BTreeMap, BTreeSet};

use koja_ir::{IREnumDecl, IRPackage, IRStructField, IRSymbol, IRType, IRVariantPayload};

/// Topologically sort every enum decl across `packages` so an enum
/// E whose payload references enum F lands after F. Used by
/// [`crate::program::compile_program`] /
/// [`crate::script::compile_script`] to drive
/// [`super::enums::define_enum_completes_and_outer`] in an order
/// where every size / alignment query sees a fully-bodied operand.
///
/// Struct fields are followed transitively because a struct payload
/// inside an enum variant pulls in the structs' nested enums too
/// (e.g. `Option<Wrapper>` where `Wrapper { inner: TokenKind }`).
/// Unresolved references (symbols missing from the program, e.g.
/// stdlib-internal types still threaded as opaque) are skipped
/// instead of panicking. They contribute no size dependency we can
/// honor here.
pub(crate) fn enums_in_dependency_order(packages: &[IRPackage]) -> Vec<&IREnumDecl> {
    let enum_index = build_enum_index(packages);
    let struct_field_index = build_struct_field_index(packages);
    let mut output: Vec<&IREnumDecl> = Vec::with_capacity(enum_index.len());
    let mut visited: BTreeSet<IRSymbol> = BTreeSet::new();
    let mut visiting: BTreeSet<IRSymbol> = BTreeSet::new();
    for decl in enum_index.values() {
        visit_enum(
            decl,
            &enum_index,
            &struct_field_index,
            &mut visited,
            &mut visiting,
            &mut output,
        );
    }
    output
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

fn visit_enum<'a>(
    decl: &'a IREnumDecl,
    enum_index: &BTreeMap<IRSymbol, &'a IREnumDecl>,
    struct_field_index: &BTreeMap<IRSymbol, &[IRStructField]>,
    visited: &mut BTreeSet<IRSymbol>,
    visiting: &mut BTreeSet<IRSymbol>,
    output: &mut Vec<&'a IREnumDecl>,
) {
    if visited.contains(&decl.symbol) || !visiting.insert(decl.symbol.clone()) {
        return;
    }
    let mut deps: BTreeSet<IRSymbol> = BTreeSet::new();
    for variant in &decl.variants {
        collect_payload_enum_refs(&variant.payload, struct_field_index, &mut deps);
    }
    for dep_symbol in deps {
        if let Some(dep_decl) = enum_index.get(&dep_symbol) {
            visit_enum(
                dep_decl,
                enum_index,
                struct_field_index,
                visited,
                visiting,
                output,
            );
        }
    }
    visiting.remove(&decl.symbol);
    visited.insert(decl.symbol.clone());
    output.push(decl);
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
            // Walk the struct's field types so a struct payload
            // (e.g. `Some(Wrapper { inner: TokenKind })`) still
            // contributes its nested enum dependencies. The
            // struct's body is already set by this point, but its
            // size depends on the inner enum being bodied.
            // Honoring the chain here keeps the chunk count
            // honest for outer-enum size computation upstream.
            if let Some(fields) = struct_field_index.get(symbol) {
                for field in *fields {
                    collect_type_enum_refs(&field.ir_type, struct_field_index, deps);
                }
            }
        }
        IRType::Union { members, .. } => {
            for member in members {
                collect_type_enum_refs(member, struct_field_index, deps);
            }
        }
        // Heap-pointer payloads: the inner type lives behind a
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
