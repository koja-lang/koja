//! The `elaborate` IR sub-pass (post-merge, post-monomorphize): the
//! last refinement before seal. It synthesizes per-type *clone*,
//! *drop*, and *deep-copy* glue for every heap-managed **composite**
//! type the program acquires, releases, or copies across a process
//! boundary, and registers it on the package set so the backend can
//! emit (and `call`) the glue without lazy backfill (northstar:
//! codegen never invokes a planner).
//!
//! ## What counts as composite glue
//!
//! Lowering ([`crate::lower::ownership`]) emits an
//! [`IRInstruction::Clone`] at every ownership acquisition and an
//! [`IRInstruction::DropLocal`] / [`IRInstruction::DropValue`] at every
//! release, for any `is_heap_managed` type. Two buckets bottom out
//! differently in the backend:
//!
//! - **Heap leaves** (`String` / `Binary` / `Bits`) and **closures**
//!   (`Function`): inline `rc++` / `rc--` (the closure env's
//!   per-capture teardown hides behind its header's `drop_fn`). No
//!   glue function, these are handled directly at the instruction.
//! - **Composites** (`List` / `Map` / `Set`, heap-owning structs /
//!   enums / tuples / unions): a `call` to the synthesized
//!   `<T>.$clone$` / `<T>.$drop$`. This pass registers those functions.
//!
//! The deep-copy family ([`IRInstruction::DeepCopy`] /
//! `<T>.$deep_copy$`, emitted at process-boundary sends) mirrors the
//! clone family bucket-for-bucket. Its discovery additionally seeds
//! from every [`FunctionKind::CopyClosureGlue`] env layout, whose
//! backend-synthesized body calls `deep_copy_T` per composite
//! capture.
//!
//! The boxed-recursive [`IRType::Indirect`] remains value-transparent.
//! The enclosing aggregate's glue acquires or releases the unboxed
//! inner value, while construction allocates the box and aggregate
//! drop or field overwrite explicitly frees it.
//!
//! A composite that turns out to own no heap (e.g. `struct Point { x:
//! Int, y: Int }`) has [`needs_drop`] `== false`, so no glue is
//! registered and the backend renders its `Clone` as a register copy
//! and its `Drop` as a no-op.
//!
//! ## Two ways a glue body is born
//!
//! - **Aggregates** (`Struct` / `Enum` / `Tuple` / `Union`):
//!   [`synthesis`] builds a full IR body: field / payload projection,
//!   per-constituent acquire / release (recursing into constituent
//!   glue via `Call`), and an aggregate rebuild for clone. These carry
//!   a non-empty CFG the backend walks like a
//!   [`FunctionKind::Regular`] body.
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
//! fields, enum payloads, tuple elements, collection elements, union
//! members, and the inner type behind a transparent `Indirect` box),
//! because a composite's glue body recurses into its constituents'
//! glue. Leaves and `Indirect` boxes themselves carry no glue family.

mod delivery;
mod exit_signal;
mod io_ready;
mod rewrite;
mod synthesis;

use std::collections::BTreeSet;

use crate::enum_decl::{IREnumDecl, IREnumVariant, IRVariantPayload};
use crate::function::{
    FunctionKind, IRBasicBlock, IRFunction, IRFunctionParam, IRIndirectSlot, IRInstruction,
    IRSymbol,
};
use crate::intrinsic_id::{IRIntrinsicId, RefMethod, ReplyToMethod};
use crate::local::IRLocalId;
use crate::mangling::{clone_glue_symbol, deep_copy_glue_symbol, drop_glue_symbol};
use crate::package::{IRPackage, insert_package_function};
use crate::struct_decl::IRStructDecl;
use crate::types::IRType;

/// Run the elaborate sub-pass over a program's package set: discover
/// the heap-managed composites that need glue, synthesize / register
/// it, then rewrite every composite acquisition / release into a glue
/// `Call`.
pub(crate) fn elaborate(packages: &mut [IRPackage]) {
    insert_indirect_overwrite_frees(packages, &mut []);
    let needed = discover_glue_types(packages, &[]);
    let deep_needed = discover_deep_copy_types(packages, &[]);
    register_all(packages, &needed, &deep_needed);
    rewrite_all(packages, &needed, &deep_needed);
    io_ready::deliver_io_ready(packages);
    exit_signal::deliver_exit_signal(packages);
}

/// Run the elaborate sub-pass for a script: same three steps as
/// [`elaborate`], but discovery also scans the inline script `body`
/// (which carries its own `Clone` / `Drop` sites outside any package
/// function) and the rewrite covers it too.
pub(crate) fn elaborate_script(packages: &mut [IRPackage], body: &mut [IRBasicBlock]) {
    insert_indirect_overwrite_frees(packages, body);
    let needed = discover_glue_types(packages, body);
    let deep_needed = discover_deep_copy_types(packages, body);
    register_all(packages, &needed, &deep_needed);
    rewrite_all(packages, &needed, &deep_needed);
    rewrite::rewrite_blocks_standalone(body, &needed, &deep_needed);
    io_ready::deliver_io_ready(packages);
    exit_signal::deliver_exit_signal(packages);
}

fn insert_indirect_overwrite_frees(packages: &mut [IRPackage], body: &mut [IRBasicBlock]) {
    let indirect_fields: BTreeSet<(IRSymbol, u32)> = packages
        .iter()
        .flat_map(|package| package.structs.values())
        .flat_map(|decl| {
            decl.fields
                .iter()
                .filter(|field| matches!(&field.ir_type, IRType::Indirect(_)))
                .map(|field| (decl.symbol.clone(), field.index))
        })
        .collect();
    if indirect_fields.is_empty() {
        return;
    }

    for blocks in packages
        .iter_mut()
        .flat_map(|package| package.functions.values_mut())
        .map(|function| function.blocks.as_mut_slice())
        .chain(std::iter::once(body))
    {
        insert_block_indirect_overwrite_frees(blocks, &indirect_fields);
    }
}

fn insert_block_indirect_overwrite_frees(
    blocks: &mut [IRBasicBlock],
    indirect_fields: &BTreeSet<(IRSymbol, u32)>,
) {
    for block in blocks {
        let mut rewritten = Vec::with_capacity(block.instructions.len());
        for instruction in block.instructions.drain(..) {
            if let IRInstruction::FieldSet {
                base,
                field_index,
                struct_symbol,
                ..
            } = &instruction
                && indirect_fields.contains(&(struct_symbol.clone(), *field_index))
            {
                rewritten.push(IRInstruction::FreeIndirect {
                    base: *base,
                    slot: IRIndirectSlot::StructField {
                        field_index: *field_index,
                        struct_symbol: struct_symbol.clone(),
                    },
                });
            }
            rewritten.push(instruction);
        }
        block.instructions = rewritten;
    }
}

fn register_all(
    packages: &mut [IRPackage],
    needed: &BTreeSet<IRType>,
    deep_needed: &BTreeSet<IRType>,
) {
    for ty in needed {
        register_glue(packages, ty);
    }
    for ty in deep_needed {
        register_deep_copy_glue(packages, ty);
    }
}

fn rewrite_all(
    packages: &mut [IRPackage],
    needed: &BTreeSet<IRType>,
    deep_needed: &BTreeSet<IRType>,
) {
    for pkg in packages.iter_mut() {
        for function in pkg.functions.values_mut() {
            rewrite::rewrite_function(function, needed, deep_needed);
        }
    }
}

/// True when `ty` owns heap storage that a `Drop` must release,
/// the precise predicate the conservative lowering-side
/// `is_heap_managed` defers to once the fully-monomorphized struct /
/// enum decls exist on the program.
///
/// Leaves and the always-heap collections answer `true` by shape.
/// Aggregates (`Struct` / `Enum` / `Union`) answer `true` iff some
/// field / payload / member does, and a `struct` of scalars needs
/// nothing. Recursion is bounded: value-level cycles are always
/// broken by an [`IRType::Indirect`] box (stamped by
/// [`crate::cycle::break_type_cycles`]), which answers `true`
/// without recursing through the named type again, and a `visited`
/// set guards against any residual revisit.
pub(crate) fn needs_drop(ty: &IRType, packages: &[IRPackage]) -> bool {
    needs_drop_seen(ty, packages, &mut BTreeSet::new())
}

fn needs_drop_seen(ty: &IRType, packages: &[IRPackage], visited: &mut BTreeSet<IRSymbol>) -> bool {
    match ty {
        IRType::Binary | IRType::Bits | IRType::String => true,
        IRType::List(_) | IRType::Map { .. } | IRType::Set(_) => true,
        // The wrapper is value-transparent, but its declaration slot
        // owns a raw box allocation independently of the inner value.
        IRType::Indirect(_) => true,
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
        IRType::Tuple(elements) => elements
            .iter()
            .any(|element| needs_drop_seen(element, packages, visited)),
        IRType::Union { members, .. } => members
            .iter()
            .any(|member| needs_drop_seen(member, packages, visited)),
        // A closure owns its heap env block. Acquisition / release is
        // always inline (`rc++` / `koja_closure_rc_dec` on the env
        // base, while the per-instance capture teardown lives behind
        // the env header's `drop_fn`, not behind per-type glue), so it
        // counts as needing drop without ever getting glue of its
        // own. See [`is_inline_managed`].
        IRType::Function { .. } => true,
        IRType::Bool
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

/// A heap-managed composite needs drop *and* is not handled inline
/// by the backend. Only these get per-type glue functions.
fn needs_glue(ty: &IRType, packages: &[IRPackage]) -> bool {
    !is_inline_managed(ty) && needs_drop(ty, packages)
}

fn is_leaf(ty: &IRType) -> bool {
    matches!(ty, IRType::Binary | IRType::Bits | IRType::String)
}

/// Heap-managed types the backends acquire / release / copy inline
/// rather than through per-type glue: the leaves (`rc++` / `rc--` /
/// `koja_heap_deep_copy` on the block base) and closures (`rc++` /
/// `koja_closure_rc_dec` / `koja_closure_deep_copy` on the env base,
/// with per-capture work behind the env header's glue pointers).
fn is_inline_managed(ty: &IRType) -> bool {
    is_leaf(ty) || matches!(ty, IRType::Function { .. })
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
/// the collection family, whose body the backend synthesizes from the
/// operand type at emit time).
fn is_aggregate(ty: &IRType) -> bool {
    matches!(
        ty,
        IRType::Enum(_) | IRType::Struct(_) | IRType::Tuple(_) | IRType::Union { .. }
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

    // The message / reply send intrinsics copy their payload across a
    // process boundary, and the runtime releases an undelivered copy
    // through the synthesized `drop_T`. Seed each send's payload type
    // so its glue is registered even when it is only ever sent as a
    // fresh owned temp (which emits no `Clone` to discover from).
    for function in packages.iter().flat_map(|pkg| pkg.functions.values()) {
        if let Some(payload) = send_intrinsic_payload(function)
            && needs_glue(payload, packages)
        {
            work.push(payload.clone());
        }
    }

    close_over_constituents(work, packages)
}

/// Walk every [`IRInstruction::DeepCopy`] to seed the set of
/// composite types that need deep-copy glue, plus every
/// [`FunctionKind::CopyClosureGlue`] env layout (the backend-
/// synthesized `$copy_env$` body calls `deep_copy_T` for each
/// composite capture), then transitively close like
/// [`discover_glue_types`].
fn discover_deep_copy_types(packages: &[IRPackage], body: &[IRBasicBlock]) -> BTreeSet<IRType> {
    let mut work: Vec<IRType> = Vec::new();
    let function_blocks = packages
        .iter()
        .flat_map(|pkg| pkg.functions.values())
        .flat_map(|function| function.blocks.iter());
    for block in function_blocks.chain(body.iter()) {
        for instruction in &block.instructions {
            if let IRInstruction::DeepCopy { ty, .. } = instruction
                && needs_glue(ty, packages)
            {
                work.push(ty.clone());
            }
        }
    }

    for function in packages.iter().flat_map(|pkg| pkg.functions.values()) {
        if let FunctionKind::CopyClosureGlue { env_layout } = &function.kind {
            for capture in env_layout {
                let capture = unbox(capture);
                if needs_glue(capture, packages) {
                    work.push(capture.clone());
                }
            }
        }
    }

    close_over_constituents(work, packages)
}

/// Transitively close a seed worklist over each composite's
/// heap-managed constituents (whose glue its body calls), keeping
/// only the types that actually carry glue.
fn close_over_constituents(mut work: Vec<IRType>, packages: &[IRPackage]) -> BTreeSet<IRType> {
    let mut needed: BTreeSet<IRType> = BTreeSet::new();
    while let Some(ty) = work.pop() {
        if !needs_glue(&ty, packages) || !needed.insert(ty.clone()) {
            continue;
        }
        for constituent in constituent_types(&ty, packages) {
            // A boxed-recursive field is cloned / dropped as its
            // unboxed inner value (the aggregate's own glue recurses),
            // so close over `inner` with no standalone `Indirect` glue.
            let constituent = unbox(&constituent).clone();
            if needs_glue(&constituent, packages) {
                work.push(constituent);
            }
        }
    }
    needed
}

/// The payload type (`M` or `R`) a message / reply send intrinsic
/// copies across a process boundary: `params[1].ty` of `Ref.cast` /
/// `Ref.call` / `Ref.send_after` / `ReplyTo.send` (`params[0]` is the
/// `self` `Ref` / `ReplyTo`). `None` for any other function.
fn send_intrinsic_payload(function: &IRFunction) -> Option<&IRType> {
    let is_send = matches!(
        function.kind,
        FunctionKind::Intrinsic(IRIntrinsicId::Ref(
            RefMethod::Call | RefMethod::Cast | RefMethod::SendAfter
        )) | FunctionKind::Intrinsic(IRIntrinsicId::ReplyTo(ReplyToMethod::Send))
    );
    is_send.then(|| function.params.get(1).map(|p| &p.ty))?
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

/// The heap-managed sub-types that `ty`'s glue body recurses into.
/// Includes aggregate members, collection elements, and transparent
/// boxes.
fn constituent_types(ty: &IRType, packages: &[IRPackage]) -> Vec<IRType> {
    match ty {
        IRType::Indirect(inner) | IRType::List(inner) | IRType::Set(inner) => {
            vec![inner.as_ref().clone()]
        }
        IRType::Map { key, value } => vec![key.as_ref().clone(), value.as_ref().clone()],
        IRType::Tuple(elements) => elements.clone(),
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

/// Register the clone + drop glue shells for `ty` (idempotent, as
/// re-registering a symbol already present is a no-op). Aggregate
/// bodies are synthesized here, while collection bodies stay empty for
/// the backend to synthesize. `Indirect` never reaches here, since it
/// is transparent and discovery closes over its inner type instead.
fn register_glue(packages: &mut [IRPackage], ty: &IRType) {
    let (clone_blocks, drop_blocks) = if is_aggregate(ty) {
        (
            synthesis::copy_body(ty, packages, synthesis::CopyMode::Clone),
            synthesis::drop_body(ty, packages),
        )
    } else {
        (Vec::new(), Vec::new())
    };
    insert_glue(
        packages,
        ty,
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
        ty,
        glue_shell(
            drop_glue_symbol(ty),
            FunctionKind::DropGlue,
            ty.clone(),
            IRType::Unit,
            drop_blocks,
        ),
    );
}

/// Register the deep-copy glue shell for `ty` (idempotent, like
/// [`register_glue`]). Same two body shapes as the clone half:
/// aggregate bodies are synthesized here, collection bodies stay
/// empty for the backend to synthesize.
fn register_deep_copy_glue(packages: &mut [IRPackage], ty: &IRType) {
    let blocks = if is_aggregate(ty) {
        synthesis::copy_body(ty, packages, synthesis::CopyMode::DeepCopy)
    } else {
        Vec::new()
    };
    insert_glue(
        packages,
        ty,
        glue_shell(
            deep_copy_glue_symbol(ty),
            FunctionKind::DeepCopyGlue,
            ty.clone(),
            ty.clone(),
            blocks,
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
        def_location: None,
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

/// Insert glue into the package that owns its concrete type.
fn insert_glue(packages: &mut [IRPackage], ty: &IRType, function: IRFunction) {
    if glue_registered(packages, &function.symbol) {
        return;
    }
    let owner = glue_owner(packages, ty);
    insert_package_function(packages, &owner, function);
}

fn glue_owner(packages: &[IRPackage], ty: &IRType) -> String {
    let owner = match ty {
        IRType::Enum(symbol) => packages
            .iter()
            .find(|package| package.enums.contains_key(symbol.mangled())),
        IRType::Struct(symbol) => packages
            .iter()
            .find(|package| package.structs.contains_key(symbol.mangled())),
        IRType::Union { mangled, .. } => packages
            .iter()
            .find(|package| package.unions.contains_key(mangled.mangled())),
        _ => return "Global".to_string(),
    };
    owner
        .map(|package| package.package.clone())
        .unwrap_or_else(|| panic!("IR elaborate: no package owns glue type `{ty:?}`"))
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

    /// Install a minimal Process-entry scaffold satisfying the
    /// program-level seal checks: a `ProcessEntryWrapper` entry whose
    /// state has registered `start` / `run` / `priority` stubs.
    /// Returns the wrapper symbol to stamp on `IRProgram::entry_point`.
    fn install_entry_scaffold(pkg: &mut IRPackage) -> IRSymbol {
        let state = sym("Test.EntryState");
        for method in ["priority", "run", "start"] {
            let stub = seed_function(
                crate::mangling::mangled_method_name(&state, &[], method, &[]),
                Vec::new(),
            );
            pkg.functions.insert(stub.symbol.clone(), stub);
        }
        let mut wrapper = seed_function(state.derived(".__entry_wrapper"), Vec::new());
        wrapper.kind = FunctionKind::ProcessEntryWrapper {
            state: IRType::Struct(state),
        };
        let wrapper_symbol = wrapper.symbol.clone();
        pkg.functions.insert(wrapper_symbol.clone(), wrapper);
        wrapper_symbol
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
            def_location: None,
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
        let entry_point = install_entry_scaffold(&mut pkg);
        let mut program = IRProgram {
            entry_point,
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
        let entry_point = install_entry_scaffold(&mut pkg);
        let mut program = IRProgram {
            entry_point,
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
    fn nested_tuple_glue_synthesizes_transitive_families_and_seals() {
        let inner_ty = IRType::Tuple(vec![IRType::String, IRType::String]);
        let outer_ty = IRType::Tuple(vec![inner_ty.clone(), IRType::Int64]);
        let left = ValueId(0);
        let right = ValueId(1);
        let inner = ValueId(2);
        let number = ValueId(3);
        let outer = ValueId(4);
        let cloned = ValueId(5);
        let deep_copied = ValueId(6);
        let seed = seed_function(
            sym("Test.seed"),
            vec![
                IRInstruction::Const {
                    dest: left,
                    value: ConstValue::String("left".to_string()),
                },
                IRInstruction::Const {
                    dest: right,
                    value: ConstValue::String("right".to_string()),
                },
                IRInstruction::TupleInit {
                    dest: inner,
                    elements: vec![left, right],
                    ty: vec![IRType::String, IRType::String],
                },
                IRInstruction::Const {
                    dest: number,
                    value: ConstValue::Int64(7),
                },
                IRInstruction::TupleInit {
                    dest: outer,
                    elements: vec![inner, number],
                    ty: vec![inner_ty.clone(), IRType::Int64],
                },
                IRInstruction::Clone {
                    dest: cloned,
                    source: outer,
                    ty: outer_ty.clone(),
                },
                IRInstruction::DeepCopy {
                    dest: deep_copied,
                    source: outer,
                    ty: outer_ty.clone(),
                },
                IRInstruction::DropValue {
                    value: outer,
                    ty: outer_ty.clone(),
                },
                IRInstruction::DropValue {
                    value: cloned,
                    ty: outer_ty.clone(),
                },
                IRInstruction::DropValue {
                    value: deep_copied,
                    ty: outer_ty.clone(),
                },
            ],
        );
        let mut pkg = empty_package("Test");
        pkg.functions.insert(seed.symbol.clone(), seed);
        let entry_point = install_entry_scaffold(&mut pkg);
        let mut program = IRProgram {
            entry_point,
            link_libraries: Vec::new(),
            packages: vec![pkg, empty_package("Global")],
        };

        elaborate_and_seal(&mut program);

        for ty in [&inner_ty, &outer_ty] {
            let clone_glue = program
                .function(clone_glue_symbol(ty).mangled())
                .expect("clone glue registered");
            assert!(matches!(clone_glue.kind, FunctionKind::CloneGlue));
            assert!(!clone_glue.blocks.is_empty());

            let drop_glue = program
                .function(drop_glue_symbol(ty).mangled())
                .expect("drop glue registered");
            assert!(matches!(drop_glue.kind, FunctionKind::DropGlue));
            assert!(!drop_glue.blocks.is_empty());

            let deep_copy_glue = program
                .function(deep_copy_glue_symbol(ty).mangled())
                .expect("deep-copy glue registered");
            assert!(matches!(deep_copy_glue.kind, FunctionKind::DeepCopyGlue));
            assert!(!deep_copy_glue.blocks.is_empty());
        }
    }

    #[test]
    fn recursive_enum_drop_releases_inner_values_before_boxes() {
        let tree = sym("Test.Tree");
        let tree_ty = IRType::Enum(tree.clone());
        let decl = IREnumDecl {
            symbol: tree.clone(),
            variants: vec![
                IREnumVariant {
                    name: "Leaf".to_string(),
                    payload: IRVariantPayload::Tuple(vec![IRType::Int64]),
                    tag: IRVariantTag(0),
                },
                IREnumVariant {
                    name: "Branch".to_string(),
                    payload: IRVariantPayload::Tuple(vec![
                        IRType::Indirect(Box::new(tree_ty.clone())),
                        IRType::Indirect(Box::new(tree_ty.clone())),
                    ]),
                    tag: IRVariantTag(1),
                },
            ],
        };
        let number = ValueId(0);
        let left = ValueId(1);
        let right = ValueId(2);
        let branch = ValueId(3);
        let cloned = ValueId(4);
        let deep_copied = ValueId(5);
        let seed = seed_function(
            sym("Test.seed"),
            vec![
                IRInstruction::Const {
                    dest: number,
                    value: ConstValue::Int64(1),
                },
                IRInstruction::EnumConstruct {
                    dest: left,
                    payload: EnumPayloadInit::Tuple(vec![number]),
                    tag: IRVariantTag(0),
                    ty: tree.clone(),
                },
                IRInstruction::EnumConstruct {
                    dest: right,
                    payload: EnumPayloadInit::Tuple(vec![number]),
                    tag: IRVariantTag(0),
                    ty: tree.clone(),
                },
                IRInstruction::EnumConstruct {
                    dest: branch,
                    payload: EnumPayloadInit::Tuple(vec![left, right]),
                    tag: IRVariantTag(1),
                    ty: tree.clone(),
                },
                IRInstruction::Clone {
                    dest: cloned,
                    source: branch,
                    ty: tree_ty.clone(),
                },
                IRInstruction::DeepCopy {
                    dest: deep_copied,
                    source: branch,
                    ty: tree_ty.clone(),
                },
                IRInstruction::DropValue {
                    value: branch,
                    ty: tree_ty.clone(),
                },
                IRInstruction::DropValue {
                    value: cloned,
                    ty: tree_ty.clone(),
                },
                IRInstruction::DropValue {
                    value: deep_copied,
                    ty: tree_ty.clone(),
                },
            ],
        );
        let mut pkg = empty_package("Test");
        pkg.enums.insert(tree.clone(), decl);
        pkg.functions.insert(seed.symbol.clone(), seed);
        let entry_point = install_entry_scaffold(&mut pkg);
        let mut program = IRProgram {
            entry_point,
            link_libraries: Vec::new(),
            packages: vec![pkg],
        };

        elaborate_and_seal(&mut program);

        for symbol in [clone_glue_symbol(&tree_ty), deep_copy_glue_symbol(&tree_ty)] {
            assert!(
                !program
                    .function(symbol.mangled())
                    .expect("copy glue registered")
                    .blocks
                    .is_empty()
            );
        }
        let drop_glue = program
            .function(drop_glue_symbol(&tree_ty).mangled())
            .expect("drop glue registered");
        let release_blocks: Vec<&IRBasicBlock> = drop_glue
            .blocks
            .iter()
            .filter(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| matches!(instruction, IRInstruction::FreeIndirect { .. }))
            })
            .collect();
        assert_eq!(release_blocks.len(), 2);
        for block in release_blocks {
            let release = block
                .instructions
                .iter()
                .position(|instruction| {
                    matches!(instruction, IRInstruction::Call { callee, .. }
                        if callee == &drop_glue_symbol(&tree_ty))
                })
                .expect("inner release");
            let free = block
                .instructions
                .iter()
                .position(|instruction| {
                    matches!(
                        instruction,
                        IRInstruction::FreeIndirect {
                            slot: IRIndirectSlot::EnumPayload { .. },
                            ..
                        }
                    )
                })
                .expect("box free");
            assert!(release < free);
        }
        assert_eq!(
            drop_glue
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| {
                    matches!(
                        instruction,
                        IRInstruction::IndirectPresent {
                            slot: IRIndirectSlot::EnumPayload { .. },
                            ..
                        }
                    )
                })
                .count(),
            2
        );
    }

    #[test]
    fn indirect_struct_overwrite_frees_replaced_box() {
        let boxed = sym("Test.Boxed");
        let boxed_ty = IRType::Struct(boxed.clone());
        let decl = IRStructDecl {
            fields: vec![IRStructField {
                index: 0,
                ir_type: IRType::Indirect(Box::new(IRType::Int64)),
                name: "value".to_string(),
            }],
            symbol: boxed.clone(),
        };
        let old_value = ValueId(0);
        let base = ValueId(1);
        let new_value = ValueId(2);
        let updated = ValueId(3);
        let seed = seed_function(
            sym("Test.seed"),
            vec![
                IRInstruction::Const {
                    dest: old_value,
                    value: ConstValue::Int64(1),
                },
                IRInstruction::StructInit {
                    dest: base,
                    fields: vec![StructFieldInit {
                        index: 0,
                        value: old_value,
                    }],
                    ty: boxed.clone(),
                },
                IRInstruction::Const {
                    dest: new_value,
                    value: ConstValue::Int64(2),
                },
                IRInstruction::FieldSet {
                    base,
                    dest: updated,
                    field_index: 0,
                    field_type: IRType::Int64,
                    struct_symbol: boxed.clone(),
                    value: new_value,
                },
                IRInstruction::DropValue {
                    value: updated,
                    ty: boxed_ty.clone(),
                },
            ],
        );
        let mut pkg = empty_package("Test");
        pkg.structs.insert(boxed.clone(), decl);
        pkg.functions.insert(seed.symbol.clone(), seed);
        let entry_point = install_entry_scaffold(&mut pkg);
        let mut program = IRProgram {
            entry_point,
            link_libraries: Vec::new(),
            packages: vec![pkg],
        };

        elaborate_and_seal(&mut program);

        let seed = program.function("Test.seed").expect("seed function");
        let overwrite = seed.blocks[0]
            .instructions
            .windows(2)
            .find(|pair| matches!(pair[1], IRInstruction::FieldSet { .. }))
            .expect("field overwrite");
        assert!(matches!(
            &overwrite[0],
            IRInstruction::FreeIndirect {
                base: free_base,
                slot: IRIndirectSlot::StructField {
                    field_index: 0,
                    struct_symbol,
                },
            } if *free_base == base && struct_symbol == &boxed
        ));

        let drop_glue = program
            .function(drop_glue_symbol(&boxed_ty).mangled())
            .expect("drop glue registered");
        assert!(
            drop_glue
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| {
                    matches!(
                        instruction,
                        IRInstruction::FreeIndirect {
                            slot: IRIndirectSlot::StructField { .. },
                            ..
                        }
                    )
                })
        );
        assert!(
            drop_glue
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| {
                    matches!(
                        instruction,
                        IRInstruction::IndirectPresent {
                            slot: IRIndirectSlot::StructField { .. },
                            ..
                        }
                    )
                })
        );
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
