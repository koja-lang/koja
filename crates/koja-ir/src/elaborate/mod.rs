//! The `elaborate` IR sub-pass (post-merge, post-monomorphize): the
//! last refinement before seal. It synthesizes per-type *clone* and
//! *drop* glue for every heap-managed **composite** type the program
//! acquires or releases, and registers it on the package set so the
//! backend can emit — and `call` — the glue without lazy backfill
//! (northstar: codegen never invokes a planner).
//!
//! ## What counts as composite glue
//!
//! Lowering ([`crate::lower::ownership`]) emits an
//! [`IRInstruction::Clone`] at every ownership acquisition and an
//! [`IRInstruction::DropLocal`] / [`IRInstruction::DropValue`] at every
//! release, for any `is_heap_managed` type. Two buckets bottom out
//! differently in the backend:
//!
//! - **Heap leaves** (`String` / `Binary` / `Bits`): inline `rc++` /
//!   `rc--`. No glue function — handled directly at the instruction.
//! - **Composites** (`List` / `Map` / `Set`, heap-owning structs /
//!   enums / unions): a `call` to the synthesized `<T>.$clone$` /
//!   `<T>.$drop$`. This pass registers those functions.
//!
//! The boxed-recursive [`IRType::Indirect`] is *transparent* — purely
//! the storage shape the cycle pass stamps on a recursive field, never
//! a value in its own right (projection unboxes, construction
//! re-boxes). It carries no glue: the enclosing aggregate's glue
//! clones / drops the unboxed inner value (recursing into the inner
//! type's glue), and the rebuild re-boxes.
//!
//! A composite that turns out to own no heap (e.g. `struct Point { x:
//! Int, y: Int }`) has [`needs_drop`] `== false`, so no glue is
//! registered and the backend renders its `Clone` as a register copy
//! and its `Drop` as a no-op.
//!
//! ## Two ways a glue body is born
//!
//! - **Aggregates** (`Struct` / `Enum` / `Union`): [`synthesis`] builds a
//!   full IR body — field / payload projection, per-constituent
//!   acquire / release (recursing into constituent glue via `Call`),
//!   and an aggregate rebuild for clone. These carry a non-empty CFG
//!   the backend walks like a [`FunctionKind::Regular`] body.
//! - **Collections** (`List` / `Map` / `Set`): the body is a
//!   runtime-shaped deep-copy / element-walk the LLVM backend
//!   synthesizes from the operand type at emit time, so the shell
//!   lowers with empty `blocks`. Eval reclaims via its host GC and
//!   never invokes either.
//!
//! ## Discovery
//!
//! Seeds are the composite operand types of every `Clone` /
//! `DropLocal` / `DropValue` across every function body (and, for
//! scripts, the inline script body). The worklist then transitively
//! pulls in each composite's heap-managed constituents (struct
//! fields, enum payloads, collection elements, union members, and the
//! inner type behind a transparent `Indirect` box), because a
//! composite's glue body recurses into its constituents' glue. Leaves
//! and `Indirect` boxes themselves are skipped — they carry no glue.

mod rewrite;
mod synthesis;

use std::collections::BTreeSet;

use crate::enum_decl::{IREnumDecl, IREnumVariant, IRVariantPayload};
use crate::function::{
    FunctionKind, IRBasicBlock, IRFunction, IRFunctionParam, IRInstruction, IRSymbol,
};
use crate::local::IRLocalId;
use crate::mangling::{clone_glue_symbol, drop_glue_symbol};
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::types::IRType;

/// Run the elaborate sub-pass over a program's package set: discover
/// the heap-managed composites that need glue, synthesize / register
/// it, then rewrite every composite acquisition / release into a glue
/// `Call`.
pub(crate) fn elaborate(packages: &mut [IRPackage]) {
    let needed = discover_glue_types(packages, &[]);
    register_all(packages, &needed);
    rewrite_all(packages, &needed);
}

/// Run the elaborate sub-pass for a script: same three steps as
/// [`elaborate`], but discovery also scans the inline script `body`
/// (which carries its own `Clone` / `Drop` sites outside any package
/// function) and the rewrite covers it too.
pub(crate) fn elaborate_script(packages: &mut [IRPackage], body: &mut [IRBasicBlock]) {
    let needed = discover_glue_types(packages, body);
    register_all(packages, &needed);
    rewrite_all(packages, &needed);
    rewrite::rewrite_blocks_standalone(body, &needed);
}

fn register_all(packages: &mut [IRPackage], needed: &BTreeSet<IRType>) {
    for ty in needed {
        register_glue(packages, ty);
    }
}

fn rewrite_all(packages: &mut [IRPackage], needed: &BTreeSet<IRType>) {
    for pkg in packages.iter_mut() {
        for function in pkg.functions.values_mut() {
            rewrite::rewrite_function(function, needed);
        }
    }
}

/// True when `ty` owns heap storage that a `Drop` must release —
/// the precise predicate the conservative lowering-side
/// `is_heap_managed` defers to once the fully-monomorphized struct /
/// enum decls exist on the program.
///
/// Leaves and the always-heap collections answer `true` by shape.
/// Aggregates (`Struct` / `Enum` / `Union`) answer `true` iff some
/// field / payload / member does — a `struct` of scalars needs
/// nothing. Recursion is bounded: value-level cycles are always
/// broken by an [`IRType::Indirect`] box (stamped by
/// [`crate::cycle::break_type_cycles`]), which answers `true`
/// without recursing through the named type again, and a `visited`
/// set guards against any residual revisit.
pub fn needs_drop(ty: &IRType, packages: &[IRPackage]) -> bool {
    needs_drop_seen(ty, packages, &mut BTreeSet::new())
}

fn needs_drop_seen(ty: &IRType, packages: &[IRPackage], visited: &mut BTreeSet<IRSymbol>) -> bool {
    match ty {
        IRType::Binary | IRType::Bits | IRType::String => true,
        IRType::List(_) | IRType::Map { .. } | IRType::Set(_) => true,
        // `Indirect` is transparent: it is purely the storage shape the
        // cycle pass stamps on a recursive field, never a value in its
        // own right (field access unboxes to `inner`, construction
        // re-boxes). Its drop-ness is the inner type's.
        IRType::Indirect(inner) => needs_drop_seen(inner, packages, visited),
        IRType::Struct(symbol) => {
            if !visited.insert(symbol.clone()) {
                return false;
            }
            find_struct(packages, symbol).is_some_and(|decl| {
                decl.fields
                    .iter()
                    .any(|field| needs_drop_seen(&field.ir_type, packages, visited))
            })
        }
        IRType::Enum(symbol) => {
            if !visited.insert(symbol.clone()) {
                return false;
            }
            find_enum(packages, symbol).is_some_and(|decl| {
                decl.variants
                    .iter()
                    .any(|variant| variant_needs_drop(variant, packages, visited))
            })
        }
        IRType::Union { members, .. } => members
            .iter()
            .any(|member| needs_drop_seen(member, packages, visited)),
        // Closures own a heap env, but their glue needs the
        // per-instance capture layout the structural type doesn't
        // carry — handled on the existing closure-specific path until
        // the closure-glue slice.
        IRType::Function { .. }
        | IRType::Bool
        | IRType::CPtr(_)
        | IRType::Float32
        | IRType::Float64
        | IRType::Int8
        | IRType::Int16
        | IRType::Int32
        | IRType::Int64
        | IRType::UInt8
        | IRType::UInt16
        | IRType::UInt32
        | IRType::UInt64
        | IRType::Unit => false,
    }
}

fn variant_needs_drop(
    variant: &IREnumVariant,
    packages: &[IRPackage],
    visited: &mut BTreeSet<IRSymbol>,
) -> bool {
    match &variant.payload {
        IRVariantPayload::Unit => false,
        IRVariantPayload::Tuple(types) => types
            .iter()
            .any(|ty| needs_drop_seen(ty, packages, visited)),
        IRVariantPayload::Struct(fields) => fields
            .iter()
            .any(|field| needs_drop_seen(&field.ir_type, packages, visited)),
    }
}

/// A heap-managed composite — needs drop *and* is not a leaf. Leaves
/// are released inline by the backend, so they get no glue function.
fn needs_glue(ty: &IRType, packages: &[IRPackage]) -> bool {
    !is_leaf(ty) && needs_drop(ty, packages)
}

fn is_leaf(ty: &IRType) -> bool {
    matches!(ty, IRType::Binary | IRType::Bits | IRType::String)
}

/// Peel a transparent [`IRType::Indirect`] box to its inner type. A
/// recursive field is stored boxed but read / written as `inner` (the
/// projection unboxes, the construction re-boxes), so every site that
/// reasons about a field's *value* type works on `inner`.
pub(super) fn unbox(ty: &IRType) -> &IRType {
    match ty {
        IRType::Indirect(inner) => inner,
        other => other,
    }
}

/// An aggregate whose glue body [`synthesis`] builds in IR (as opposed to
/// the collection / `Indirect` family, whose body the backend
/// synthesizes from the operand type at emit time).
fn is_aggregate(ty: &IRType) -> bool {
    matches!(
        ty,
        IRType::Enum(_) | IRType::Struct(_) | IRType::Union { .. }
    )
}

/// Walk every `Clone` / `DropLocal` / `DropValue` to seed the set of
/// composite types that need glue, then transitively close over each
/// composite's heap-managed constituents (whose glue its body calls).
fn discover_glue_types(packages: &[IRPackage], body: &[IRBasicBlock]) -> BTreeSet<IRType> {
    let mut work: Vec<IRType> = Vec::new();
    let function_blocks = packages
        .iter()
        .flat_map(|pkg| pkg.functions.values())
        .flat_map(|function| function.blocks.iter());
    for block in function_blocks.chain(body.iter()) {
        for instruction in &block.instructions {
            if let Some(ty) = clone_or_drop_type(instruction)
                && needs_glue(ty, packages)
            {
                work.push(ty.clone());
            }
        }
    }

    let mut needed: BTreeSet<IRType> = BTreeSet::new();
    while let Some(ty) = work.pop() {
        if !needs_glue(&ty, packages) || !needed.insert(ty.clone()) {
            continue;
        }
        for constituent in constituent_types(&ty, packages) {
            // A boxed-recursive field is cloned / dropped as its
            // unboxed inner value (the aggregate's own glue recurses),
            // so close over `inner` — no standalone `Indirect` glue.
            let constituent = unbox(&constituent).clone();
            if needs_glue(&constituent, packages) {
                work.push(constituent);
            }
        }
    }
    needed
}

/// The operand type of a `Clone` / `DropLocal` / `DropValue`, or
/// `None` for any other instruction.
fn clone_or_drop_type(instruction: &IRInstruction) -> Option<&IRType> {
    match instruction {
        IRInstruction::Clone { ty, .. }
        | IRInstruction::DropLocal { ty, .. }
        | IRInstruction::DropValue { ty, .. } => Some(ty),
        _ => None,
    }
}

/// The heap-managed sub-types `ty`'s glue body recurses into: struct
/// fields, enum payloads, union members, collection elements, and the
/// `Indirect` inner type.
fn constituent_types(ty: &IRType, packages: &[IRPackage]) -> Vec<IRType> {
    match ty {
        IRType::Indirect(inner) | IRType::List(inner) | IRType::Set(inner) => {
            vec![inner.as_ref().clone()]
        }
        IRType::Map { key, value } => vec![key.as_ref().clone(), value.as_ref().clone()],
        IRType::Union { members, .. } => members.clone(),
        IRType::Struct(symbol) => find_struct(packages, symbol)
            .map(|decl| decl.fields.iter().map(|f| f.ir_type.clone()).collect())
            .unwrap_or_default(),
        IRType::Enum(symbol) => find_enum(packages, symbol)
            .map(|decl| {
                decl.variants
                    .iter()
                    .flat_map(|variant| match &variant.payload {
                        IRVariantPayload::Unit => Vec::new(),
                        IRVariantPayload::Tuple(types) => types.clone(),
                        IRVariantPayload::Struct(fields) => {
                            fields.iter().map(|f| f.ir_type.clone()).collect()
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn find_struct<'a>(packages: &'a [IRPackage], symbol: &IRSymbol) -> Option<&'a IRStructDecl> {
    packages
        .iter()
        .find_map(|pkg| pkg.structs.get(symbol.mangled()))
}

fn find_enum<'a>(packages: &'a [IRPackage], symbol: &IRSymbol) -> Option<&'a IREnumDecl> {
    packages
        .iter()
        .find_map(|pkg| pkg.enums.get(symbol.mangled()))
}

fn glue_registered(packages: &[IRPackage], symbol: &IRSymbol) -> bool {
    packages
        .iter()
        .any(|pkg| pkg.functions.contains_key(symbol.mangled()))
}

/// Register the clone + drop glue shells for `ty` (idempotent —
/// re-registering a symbol already present is a no-op). Aggregate
/// bodies are synthesized here; collection bodies stay empty for the
/// backend to synthesize. `Indirect` never reaches here — it is
/// transparent and discovery closes over its inner type instead.
fn register_glue(packages: &mut [IRPackage], ty: &IRType) {
    let (clone_blocks, drop_blocks) = if is_aggregate(ty) {
        (
            synthesis::clone_body(ty, packages),
            synthesis::drop_body(ty, packages),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    insert_glue(
        packages,
        glue_shell(
            clone_glue_symbol(ty),
            FunctionKind::CloneGlue,
            ty.clone(),
            ty.clone(),
            clone_blocks,
        ),
    );
    insert_glue(
        packages,
        glue_shell(
            drop_glue_symbol(ty),
            FunctionKind::DropGlue,
            ty.clone(),
            IRType::Unit,
            drop_blocks,
        ),
    );
}

/// Build a glue function: one `self: operand_ty` parameter, the given
/// `return_type`, and `blocks` (empty for collection glue, a full CFG
/// for aggregate glue).
fn glue_shell(
    symbol: IRSymbol,
    kind: FunctionKind,
    operand_ty: IRType,
    return_type: IRType,
    blocks: Vec<IRBasicBlock>,
) -> IRFunction {
    IRFunction {
        blocks,
        kind,
        params: vec![IRFunctionParam {
            id: synthesis::SELF_VALUE,
            local_id: IRLocalId::synthetic_placeholder(),
            ty: operand_ty,
        }],
        return_type,
        symbol,
    }
}

/// Insert `function` into the package its symbol prefix names, or the
/// `packages[0]` fallback when the prefix matches no package (the
/// structural-collection glue case, whose synthetic symbol has no
/// real package root). Idempotent: a symbol already registered
/// anywhere is left untouched.
fn insert_glue(packages: &mut [IRPackage], function: IRFunction) {
    if glue_registered(packages, &function.symbol) {
        return;
    }
    let symbol = function.symbol.mangled();
    let prefix = symbol.split('.').next().unwrap_or(symbol);
    let index = packages
        .iter()
        .position(|pkg| pkg.package == prefix)
        .unwrap_or(0);
    let owner = packages
        .get_mut(index)
        .expect("IR elaborate: no IRPackage available to host synthesized glue");
    owner.functions.insert(function.symbol.clone(), function);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::IRProgram;
    use crate::enum_decl::{EnumPayloadInit, IREnumVariant, IRVariantTag};
    use crate::function::{IRBasicBlock, IRBlockId, IRTerminator};
    use crate::seal::seal_program;
    use crate::struct_decl::{IRStructField, StructFieldInit};
    use crate::types::{ConstValue, ValueId};

    fn sym(name: &str) -> IRSymbol {
        IRSymbol::synthetic(name.to_string())
    }

    fn string_field(index: u32, name: &str) -> IRStructField {
        IRStructField {
            index,
            ir_type: IRType::String,
            name: name.to_string(),
        }
    }

    /// Wrap one already-built block into a `Regular` seed function so
    /// `elaborate` discovers the `Clone` / `Drop` sites it carries and
    /// `seal_program` validates it alongside the synthesized glue.
    fn seed_function(symbol: IRSymbol, instructions: Vec<IRInstruction>) -> IRFunction {
        IRFunction {
            blocks: vec![IRBasicBlock {
                id: IRBlockId(0),
                label: "entry".to_string(),
                params: Vec::new(),
                instructions,
                terminator: IRTerminator::Return { value: None },
            }],
            kind: FunctionKind::Regular,
            params: Vec::new(),
            return_type: IRType::Unit,
            symbol,
        }
    }

    fn empty_package(name: &str) -> IRPackage {
        IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions: BTreeMap::new(),
            package: name.to_string(),
            structs: BTreeMap::new(),
            unions: BTreeMap::new(),
        }
    }

    fn elaborate_and_seal(program: &mut IRProgram) {
        elaborate(&mut program.packages);
        seal_program(program);
    }

    #[test]
    fn struct_glue_synthesizes_and_seals() {
        let point = sym("Test.Point");
        let point_ty = IRType::Struct(point.clone());
        let decl = IRStructDecl {
            fields: vec![string_field(0, "name"), string_field(1, "label")],
            symbol: point.clone(),
        };
        // seed: p = Point{ "", "" }; pc = clone(p); drop(p); drop(pc)
        let p = ValueId(0);
        let pc = ValueId(1);
        let s0 = ValueId(2);
        let s1 = ValueId(3);
        let seed = seed_function(
            sym("Test.seed"),
            vec![
                IRInstruction::Const {
                    dest: s0,
                    value: ConstValue::String(String::new()),
                },
                IRInstruction::Const {
                    dest: s1,
                    value: ConstValue::String(String::new()),
                },
                IRInstruction::StructInit {
                    dest: p,
                    fields: vec![
                        StructFieldInit {
                            index: 0,
                            value: s0,
                        },
                        StructFieldInit {
                            index: 1,
                            value: s1,
                        },
                    ],
                    ty: point.clone(),
                },
                IRInstruction::Clone {
                    dest: pc,
                    source: p,
                    ty: point_ty.clone(),
                },
                IRInstruction::DropValue {
                    value: p,
                    ty: point_ty.clone(),
                },
                IRInstruction::DropValue {
                    value: pc,
                    ty: point_ty.clone(),
                },
            ],
        );
        let mut pkg = empty_package("Test");
        pkg.structs.insert(point.clone(), decl);
        pkg.functions.insert(seed.symbol.clone(), seed);
        let mut program = IRProgram {
            entry_point: sym("Test.seed"),
            link_libraries: Vec::new(),
            packages: vec![pkg],
        };

        elaborate_and_seal(&mut program);

        let clone_glue = program
            .function(clone_glue_symbol(&point_ty).mangled())
            .expect("clone glue registered");
        assert!(matches!(clone_glue.kind, FunctionKind::CloneGlue));
        assert!(!clone_glue.blocks.is_empty(), "aggregate clone has a body");
        let drop_glue = program
            .function(drop_glue_symbol(&point_ty).mangled())
            .expect("drop glue registered");
        assert!(matches!(drop_glue.kind, FunctionKind::DropGlue));
        assert!(!drop_glue.blocks.is_empty(), "aggregate drop has a body");
    }

    #[test]
    fn enum_glue_synthesizes_and_seals() {
        let option = sym("Test.Opt");
        let option_ty = IRType::Enum(option.clone());
        let decl = IREnumDecl {
            symbol: option.clone(),
            variants: vec![
                IREnumVariant {
                    name: "None".to_string(),
                    payload: IRVariantPayload::Unit,
                    tag: IRVariantTag(0),
                },
                IREnumVariant {
                    name: "Some".to_string(),
                    payload: IRVariantPayload::Tuple(vec![IRType::String]),
                    tag: IRVariantTag(1),
                },
            ],
        };
        // seed: payload = ""; e = Opt.Some(payload); ec = clone(e);
        //       drop(e); drop(ec)
        let payload = ValueId(0);
        let e = ValueId(1);
        let ec = ValueId(2);
        let seed = seed_function(
            sym("Test.seed"),
            vec![
                IRInstruction::Const {
                    dest: payload,
                    value: ConstValue::String(String::new()),
                },
                IRInstruction::EnumConstruct {
                    dest: e,
                    payload: EnumPayloadInit::Tuple(vec![payload]),
                    tag: IRVariantTag(1),
                    ty: option.clone(),
                },
                IRInstruction::Clone {
                    dest: ec,
                    source: e,
                    ty: option_ty.clone(),
                },
                IRInstruction::DropValue {
                    value: e,
                    ty: option_ty.clone(),
                },
                IRInstruction::DropValue {
                    value: ec,
                    ty: option_ty.clone(),
                },
            ],
        );
        let mut pkg = empty_package("Test");
        pkg.enums.insert(option.clone(), decl);
        pkg.functions.insert(seed.symbol.clone(), seed);
        let mut program = IRProgram {
            entry_point: sym("Test.seed"),
            link_libraries: Vec::new(),
            packages: vec![pkg],
        };

        elaborate_and_seal(&mut program);

        let clone_glue = program
            .function(clone_glue_symbol(&option_ty).mangled())
            .expect("clone glue registered");
        assert!(matches!(clone_glue.kind, FunctionKind::CloneGlue));
        // entry + join + one body per variant.
        assert_eq!(clone_glue.blocks.len(), 4);
        let drop_glue = program
            .function(drop_glue_symbol(&option_ty).mangled())
            .expect("drop glue registered");
        assert!(matches!(drop_glue.kind, FunctionKind::DropGlue));
    }

    #[test]
    fn scalar_struct_needs_no_glue() {
        // struct of scalars: needs_drop == false, so no glue and no
        // Clone/Drop seed is discoverable.
        let pair = sym("Test.IntPair");
        let pair_ty = IRType::Struct(pair.clone());
        assert!(!needs_drop(
            &pair_ty,
            &[{
                let mut pkg = empty_package("Test");
                pkg.structs.insert(
                    pair.clone(),
                    IRStructDecl {
                        fields: vec![
                            IRStructField {
                                index: 0,
                                ir_type: IRType::Int64,
                                name: "a".to_string(),
                            },
                            IRStructField {
                                index: 1,
                                ir_type: IRType::Int64,
                                name: "b".to_string(),
                            },
                        ],
                        symbol: pair.clone(),
                    },
                );
                pkg
            }],
        ));
    }

    #[test]
    fn glue_symbols_are_dollar_fenced_and_distinct() {
        let clone = clone_glue_symbol(&IRType::List(Box::new(IRType::String)));
        let drop = drop_glue_symbol(&IRType::List(Box::new(IRType::String)));
        assert!(clone.mangled().ends_with(".$clone$"));
        assert!(drop.mangled().ends_with(".$drop$"));
        assert_ne!(clone, drop);
    }
}
