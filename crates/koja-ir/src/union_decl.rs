//! Lowered union declarations and the helper that precomputes
//! their tagged-runtime payload size.
//!
//! A surface union `A | B | C` lowers to an `IRType::Union { mangled,
//! members }` head plus a per-program [`IRUnionDecl`] entry keyed by
//! the same `mangled` symbol. Backends look the decl up to discover
//! `max_payload_size`, the byte width of the largest member, which
//! determines the trailing `[N x i8]` payload buffer in the
//! `{ i8, [N x i8] }` LLVM struct layout.
//!
//! Every distinct canonical surface union (sorted, deduped, alias-
//! peeled member set) yields the same `mangled` symbol and a single
//! shared decl.

use std::collections::BTreeMap;

use crate::enum_decl::{IREnumDecl, IRVariantPayload};
use crate::function::{
    BlockParam, IRBasicBlock, IRFunction, IRInstruction, IRSymbol, IRTerminator,
};
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::types::IRType;

/// A lowered union declaration. `members` is the canonical
/// (sorted) member type vector inherited from the surface
/// `ResolvedType::Union`, and `max_payload_size` is the byte width of
/// the largest member as computed by [`size_in_bytes`]. Backends
/// consult `max_payload_size` to size the trailing `[N x i8]`
/// payload buffer.
#[derive(Debug, Clone)]
pub struct IRUnionDecl {
    pub max_payload_size: u32,
    pub members: Vec<IRType>,
    pub symbol: IRSymbol,
}

/// Conservative byte-size walker used to size the trailing
/// `[N x i8]` payload buffer in a union's `{ i8, [N x i8] }` LLVM
/// struct layout.
///
/// Mirrors the LLVM backend's per-shape ABI sizing on a 64-bit
/// host. Primitives match their bit width, pointer-shaped values
/// use the host pointer size, and structs sum their fields *with
/// padding* to natural alignment. Enums use the same `{ i8 tag +
/// align padding + max-payload }` blob the LLVM enum layout
/// produces, with the outer rounded up to its max-align stride.
/// Nested unions resolve through `unions`. If the inner decl
/// isn't yet registered, fall back to `1 + max(member_size)` as
/// a conservative upper bound so the caller's first sizing pass
/// still produces a usable number that a later
/// [`refine_nested_union_sizes`] pass can settle.
pub(crate) fn size_in_bytes(
    ty: &IRType,
    structs: &BTreeMap<IRSymbol, IRStructDecl>,
    enums: &BTreeMap<IRSymbol, IREnumDecl>,
    unions: &BTreeMap<IRSymbol, IRUnionDecl>,
) -> u32 {
    let (size, _align) = size_and_align(ty, structs, enums, unions);
    size
}

/// `(size, align)` for an [`IRType`] using the same ABI rules
/// `size_in_bytes` documents. Internal helper exported so the
/// struct / enum walkers can apply field padding correctly.
fn size_and_align(
    ty: &IRType,
    structs: &BTreeMap<IRSymbol, IRStructDecl>,
    enums: &BTreeMap<IRSymbol, IREnumDecl>,
    unions: &BTreeMap<IRSymbol, IRUnionDecl>,
) -> (u32, u32) {
    const PTR_BYTES: u32 = 8;
    match ty {
        IRType::Bool | IRType::Int8 | IRType::UInt8 => (1, 1),
        IRType::Int16 | IRType::UInt16 => (2, 2),
        IRType::Float32 | IRType::Int32 | IRType::UInt32 => (4, 4),
        IRType::Float64 | IRType::Int64 | IRType::UInt64 => (8, 8),
        IRType::Unit => (0, 1),
        IRType::Binary | IRType::Bits | IRType::CPtr(_) | IRType::Indirect(_) | IRType::String => {
            (PTR_BYTES, PTR_BYTES)
        }
        // Fat values whose LLVM shapes are wider than one pointer:
        // closure `{ fn_ptr, env_ptr }`, list `{ buf_ptr, len, cap }`,
        // hashtable `{ entries_ptr, states_ptr, len, cap }`. Must
        // mirror `koja-ir-llvm`'s value types or a union payload
        // buffer truncates the value it stores.
        IRType::Function { .. } => (2 * PTR_BYTES, PTR_BYTES),
        IRType::List(_) => (3 * PTR_BYTES, PTR_BYTES),
        IRType::Map { .. } | IRType::Set(_) => (4 * PTR_BYTES, PTR_BYTES),
        IRType::Struct(symbol) => match structs.get(symbol) {
            Some(decl) => sum_fields(
                decl.fields.iter().map(|f| &f.ir_type),
                structs,
                enums,
                unions,
            ),
            None => (PTR_BYTES, PTR_BYTES),
        },
        IRType::Tuple(elements) => sum_fields(elements.iter(), structs, enums, unions),
        IRType::Enum(symbol) => match enums.get(symbol) {
            Some(decl) => enum_size(decl, structs, enums, unions),
            None => (PTR_BYTES, PTR_BYTES),
        },
        IRType::Union { mangled, members } => match unions.get(mangled) {
            Some(decl) => (1 + decl.max_payload_size, 1),
            None => {
                let payload = members
                    .iter()
                    .map(|m| size_in_bytes(m, structs, enums, unions))
                    .max()
                    .unwrap_or(0);
                (1 + payload, 1)
            }
        },
    }
}

/// Sum a sequence of field types into `(size, align)` accounting
/// for natural alignment padding and trailing pad-to-align.
/// Mirrors the layout `inkwell` would produce for a
/// `context.struct_type(&[...], false)` (non-packed) call.
fn sum_fields<'a, I>(
    fields: I,
    structs: &BTreeMap<IRSymbol, IRStructDecl>,
    enums: &BTreeMap<IRSymbol, IREnumDecl>,
    unions: &BTreeMap<IRSymbol, IRUnionDecl>,
) -> (u32, u32)
where
    I: IntoIterator<Item = &'a IRType>,
{
    let mut size = 0u32;
    let mut align = 1u32;
    for field in fields {
        let (field_size, field_align) = size_and_align(field, structs, enums, unions);
        size = round_up(size, field_align);
        size += field_size;
        align = align.max(field_align);
    }
    size = round_up(size, align);
    (size, align)
}

/// Enum payload sizing mirrors `koja-ir-llvm`'s layout: each
/// variant is a `{ i8 tag, [pad x i8], payload }` blob, and the outer
/// is `{ [count x iN] }` where `N = max_align * 8` and the byte
/// count is `count * max_align >= max_complete_size`. Returns the
/// outer's `(size, align)`.
fn enum_size(
    decl: &IREnumDecl,
    structs: &BTreeMap<IRSymbol, IRStructDecl>,
    enums: &BTreeMap<IRSymbol, IREnumDecl>,
    unions: &BTreeMap<IRSymbol, IRUnionDecl>,
) -> (u32, u32) {
    let mut max_complete_size = 0u32;
    let mut max_complete_align = 1u32;
    for variant in &decl.variants {
        let (payload_size, payload_align) = match &variant.payload {
            IRVariantPayload::Unit => (0, 1),
            IRVariantPayload::Tuple(types) => sum_fields(types.iter(), structs, enums, unions),
            IRVariantPayload::Struct(fields) => {
                sum_fields(fields.iter().map(|f| &f.ir_type), structs, enums, unions)
            }
        };
        let variant_align = payload_align.max(1);
        // Complete = `{ i8 tag, [pad x i8], payload }`. The tag occupies
        // 1 byte, payload starts at its natural alignment, total
        // rounds up to the variant's max align so the outer's
        // stride absorbs it.
        let pad_before_payload = round_up(1, variant_align) - 1;
        let variant_size = round_up(1 + pad_before_payload + payload_size, variant_align);
        max_complete_size = max_complete_size.max(variant_size);
        max_complete_align = max_complete_align.max(variant_align);
    }
    let outer_size = round_up(max_complete_size, max_complete_align);
    (outer_size, max_complete_align)
}

fn round_up(value: u32, alignment: u32) -> u32 {
    let alignment = alignment.max(1);
    value.div_ceil(alignment) * alignment
}

/// Walk every IR type referenced in `packages` (and, for script
/// mode, the top-level `script_blocks`) and register one
/// [`IRUnionDecl`] per distinct mangled `IRType::Union` head into
/// the package that first observed it. Script-only unions land in
/// the first package. Cross-package lookup goes through
/// [`crate::IRProgram::union_decl`], so where the decl physically
/// lives doesn't matter for backends. What matters is that every
/// observed mangled symbol has exactly one entry.
pub(crate) fn discover_unions(packages: &mut [IRPackage], script_blocks: &[IRBasicBlock]) {
    let struct_index: BTreeMap<IRSymbol, IRStructDecl> = packages
        .iter()
        .flat_map(|pkg| {
            pkg.structs
                .iter()
                .map(|(symbol, decl)| (symbol.clone(), decl.clone()))
        })
        .collect();
    let enum_index: BTreeMap<IRSymbol, IREnumDecl> = packages
        .iter()
        .flat_map(|pkg| {
            pkg.enums
                .iter()
                .map(|(symbol, decl)| (symbol.clone(), decl.clone()))
        })
        .collect();
    let mut seen: BTreeMap<IRSymbol, usize> = BTreeMap::new();
    let mut staged: Vec<BTreeMap<IRSymbol, IRType>> =
        packages.iter().map(|_| BTreeMap::new()).collect();
    for (idx, pkg) in packages.iter().enumerate() {
        let mut local: BTreeMap<IRSymbol, IRType> = BTreeMap::new();
        for decl in pkg.structs.values() {
            for field in &decl.fields {
                walk_type(&field.ir_type, &mut local);
            }
        }
        for decl in pkg.enums.values() {
            for variant in &decl.variants {
                walk_variant_payload(&variant.payload, &mut local);
            }
        }
        for function in pkg.functions.values() {
            walk_function(function, &mut local);
        }
        for (mangled, ir_type) in local {
            if seen.contains_key(&mangled) {
                continue;
            }
            seen.insert(mangled.clone(), idx);
            staged[idx].insert(mangled, ir_type);
        }
    }
    if !script_blocks.is_empty() && !packages.is_empty() {
        let mut local: BTreeMap<IRSymbol, IRType> = BTreeMap::new();
        for block in script_blocks {
            walk_block(block, &mut local);
        }
        for (mangled, ir_type) in local {
            if !seen.contains_key(&mangled) {
                seen.insert(mangled.clone(), 0);
                staged[0].insert(mangled, ir_type);
            }
        }
    }
    let empty_unions: BTreeMap<IRSymbol, IRUnionDecl> = BTreeMap::new();
    let mut decls_by_pkg: Vec<BTreeMap<IRSymbol, IRUnionDecl>> =
        packages.iter().map(|_| BTreeMap::new()).collect();
    for (idx, locals) in staged.iter().enumerate() {
        for (mangled, ir_type) in locals {
            let IRType::Union { members, .. } = ir_type else {
                continue;
            };
            let max_payload_size = members
                .iter()
                .map(|m| size_in_bytes(m, &struct_index, &enum_index, &empty_unions))
                .max()
                .unwrap_or(0);
            decls_by_pkg[idx].insert(
                mangled.clone(),
                IRUnionDecl {
                    max_payload_size,
                    members: members.clone(),
                    symbol: mangled.clone(),
                },
            );
        }
    }
    refine_nested_union_sizes(&mut decls_by_pkg, &struct_index, &enum_index);
    for (pkg, unions) in packages.iter_mut().zip(decls_by_pkg) {
        pkg.unions.extend(unions);
    }
}

/// Recompute `max_payload_size` for any union whose member set
/// includes another union. Iterates until fixpoint so chained
/// `(A | B) | C` references settle into stable byte counts.
fn refine_nested_union_sizes(
    staged: &mut [BTreeMap<IRSymbol, IRUnionDecl>],
    struct_index: &BTreeMap<IRSymbol, IRStructDecl>,
    enum_index: &BTreeMap<IRSymbol, IREnumDecl>,
) {
    loop {
        let snapshot: BTreeMap<IRSymbol, IRUnionDecl> = staged
            .iter()
            .flat_map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())))
            .collect();
        let mut changed = false;
        for unions in staged.iter_mut() {
            for decl in unions.values_mut() {
                let recomputed = decl
                    .members
                    .iter()
                    .map(|m| size_in_bytes(m, struct_index, enum_index, &snapshot))
                    .max()
                    .unwrap_or(0);
                if recomputed != decl.max_payload_size {
                    decl.max_payload_size = recomputed;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn walk_function(function: &IRFunction, out: &mut BTreeMap<IRSymbol, IRType>) {
    for param in &function.params {
        walk_type(&param.ty, out);
    }
    walk_type(&function.return_type, out);
    for block in &function.blocks {
        walk_block(block, out);
    }
}

fn walk_block(block: &IRBasicBlock, out: &mut BTreeMap<IRSymbol, IRType>) {
    for param in &block.params {
        walk_block_param(param, out);
    }
    for instruction in &block.instructions {
        walk_instruction(instruction, out);
    }
    walk_terminator(&block.terminator, out);
}

fn walk_block_param(param: &BlockParam, out: &mut BTreeMap<IRSymbol, IRType>) {
    walk_type(&param.ty, out);
}

fn walk_instruction(instruction: &IRInstruction, out: &mut BTreeMap<IRSymbol, IRType>) {
    match instruction {
        IRInstruction::CallClosure { result_ty, .. } => walk_type(result_ty, out),
        IRInstruction::Clone { ty, .. }
        | IRInstruction::DeepCopy { ty, .. }
        | IRInstruction::DropLocal { ty, .. }
        | IRInstruction::DropValue { ty, .. } => walk_type(ty, out),
        IRInstruction::EnumPayloadFieldGet { field_type, .. } => walk_type(field_type, out),
        IRInstruction::FieldGet { field_type, .. } | IRInstruction::FieldSet { field_type, .. } => {
            walk_type(field_type, out)
        }
        IRInstruction::LoadCapture { ty, .. }
        | IRInstruction::LoadConst { ty, .. }
        | IRInstruction::LocalDecl { ty, .. }
        | IRInstruction::LocalRead { ty, .. }
        | IRInstruction::MakeClosure { ty, .. } => walk_type(ty, out),
        IRInstruction::Receive {
            arms, result_type, ..
        } => {
            walk_type(result_type, out);
            for arm in arms {
                walk_type(&arm.payload_type, out);
            }
        }
        IRInstruction::NumericWiden { from, to, .. } => {
            walk_type(from, out);
            walk_type(to, out);
        }
        IRInstruction::Spawn { config_type, .. } => walk_type(config_type, out),
        IRInstruction::TupleGet { element_type, .. } => walk_type(element_type, out),
        IRInstruction::TupleInit { ty, .. } => {
            for element in ty {
                walk_type(element, out);
            }
        }
        IRInstruction::UnionWrap {
            member_type, ty, ..
        }
        | IRInstruction::UnionPayloadGet {
            member_type, ty, ..
        } => {
            walk_type(member_type, out);
            walk_type(ty, out);
        }
        IRInstruction::UnionTagGet { ty, .. } => walk_type(ty, out),
        IRInstruction::BinaryMatch { segments, .. } => {
            for segment in segments {
                walk_binary_pattern(segment, out);
            }
        }
        IRInstruction::BinaryConstruct { .. }
        | IRInstruction::BinaryOp { .. }
        | IRInstruction::Call { .. }
        | IRInstruction::Concat { .. }
        | IRInstruction::Const { .. }
        | IRInstruction::EnumConstruct { .. }
        | IRInstruction::EnumTagGet { .. }
        | IRInstruction::LocalWrite { .. }
        | IRInstruction::ProcessExit { .. }
        | IRInstruction::SetPriority { .. }
        | IRInstruction::StructInit { .. }
        | IRInstruction::UnaryOp { .. }
        | IRInstruction::YieldCheck => {}
    }
}

/// Per-segment IRType walker for the binary-pattern instruction.
/// Only [`LoweredBinaryPattern::BindInt`] / [`LoweredBinaryPattern::GreedyTail`]
/// carry types. The rest are pure shape + bit-offset metadata.
fn walk_binary_pattern(
    segment: &crate::types::LoweredBinaryPattern,
    out: &mut BTreeMap<IRSymbol, IRType>,
) {
    use crate::types::LoweredBinaryPattern;
    match segment {
        LoweredBinaryPattern::BindInt { ty, .. } | LoweredBinaryPattern::GreedyTail { ty, .. } => {
            walk_type(ty, out);
        }
        LoweredBinaryPattern::LiteralInt { .. }
        | LoweredBinaryPattern::LiteralBytes { .. }
        | LoweredBinaryPattern::Discard { .. } => {}
    }
}

fn walk_terminator(terminator: &IRTerminator, _out: &mut BTreeMap<IRSymbol, IRType>) {
    match terminator {
        IRTerminator::Branch(_)
        | IRTerminator::CondBranch { .. }
        | IRTerminator::Return { .. }
        | IRTerminator::TailCall { .. }
        | IRTerminator::Unreachable => {}
    }
}

fn walk_variant_payload(payload: &IRVariantPayload, out: &mut BTreeMap<IRSymbol, IRType>) {
    match payload {
        IRVariantPayload::Unit => {}
        IRVariantPayload::Tuple(types) => {
            for ty in types {
                walk_type(ty, out);
            }
        }
        IRVariantPayload::Struct(fields) => {
            for field in fields {
                walk_type(&field.ir_type, out);
            }
        }
    }
}

fn walk_type(ty: &IRType, out: &mut BTreeMap<IRSymbol, IRType>) {
    match ty {
        IRType::Binary
        | IRType::Bits
        | IRType::Bool
        | IRType::Enum(_)
        | IRType::Float32
        | IRType::Float64
        | IRType::Int8
        | IRType::Int16
        | IRType::Int32
        | IRType::Int64
        | IRType::String
        | IRType::Struct(_)
        | IRType::UInt8
        | IRType::UInt16
        | IRType::UInt32
        | IRType::UInt64
        | IRType::Unit => {}
        IRType::CPtr(inner)
        | IRType::Indirect(inner)
        | IRType::List(inner)
        | IRType::Set(inner) => walk_type(inner, out),
        IRType::Map { key, value } => {
            walk_type(key, out);
            walk_type(value, out);
        }
        IRType::Function { params, ret, .. } => {
            for param in params {
                walk_type(param, out);
            }
            walk_type(ret, out);
        }
        IRType::Tuple(elements) => {
            for element in elements {
                walk_type(element, out);
            }
        }
        IRType::Union { mangled, members } => {
            for member in members {
                walk_type(member, out);
            }
            out.entry(mangled.clone()).or_insert_with(|| ty.clone());
        }
    }
}
