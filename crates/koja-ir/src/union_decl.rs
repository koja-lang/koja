//! Lowered union declarations and the helper that precomputes
//! their tagged-runtime payload size.
//!
//! A surface union `A | B | C` lowers to an `IRType::Union` that
//! holds a symbol, plus an [`IRUnionDecl`] keyed by the same symbol.
//! The decl holds the members and `max_payload_size`, the byte width
//! of the largest member, which determines the trailing `[M x i64]`
//! payload buffer in the `{ i64, [M x i64] }` LLVM struct layout.
//! [`union_members`] reads the members.
//!
//! Every distinct canonical surface union (sorted, deduped, alias-
//! peeled member set) yields the same symbol. Each package that
//! names the union declares it, and the copies are equal.
//!
//! The union of a recursive `type` alias has the alias name as its
//! symbol. Only the package of the alias declares it, and its decl
//! exists before function lowering starts.

use std::collections::BTreeMap;

use crate::declarations::Declarations;
use crate::enum_decl::{IREnumDecl, IRVariantPayload};
use crate::function::IRSymbol;
use crate::package::IRPackage;
use crate::types::IRType;

/// A lowered union declaration. `members` is the canonical
/// (sorted) member type vector inherited from the surface
/// `ResolvedType::Union`, and `max_payload_size` is the byte width of
/// the largest member as computed by [`size_in_bytes`]. Backends
/// consult `max_payload_size` to size the trailing `[M x i64]`
/// payload buffer.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct IRUnionDecl {
    pub max_payload_size: u32,
    pub members: Vec<IRType>,
    pub symbol: IRSymbol,
}

/// The members of a union type in canonical order, or `None` when
/// `ty` is not a union. The members live on the declaration, which
/// `declaration` finds by the union symbol. Panics when it finds
/// none, because lowering declares each union when a type first
/// names it.
pub(crate) fn union_members<'a>(
    ty: &IRType,
    declaration: impl FnOnce(&IRSymbol) -> Option<&'a IRUnionDecl>,
) -> Option<&'a [IRType]> {
    let IRType::Union(symbol) = ty else {
        return None;
    };
    let Some(decl) = declaration(symbol) else {
        panic!("IR: union `{symbol}` has no declaration");
    };
    Some(&decl.members)
}

/// Conservative byte-size walker used to size the trailing
/// `[M x i64]` payload buffer in a union's `{ i64, [M x i64] }` LLVM
/// struct layout.
///
/// Mirrors the LLVM backend's per-shape ABI sizing on a 64-bit
/// host. Primitives match their bit width, value-position `Unit`
/// uses LLVM's inert `i8` placeholder, pointer-shaped values use
/// the host pointer size, and structs sum their fields *with
/// padding* to natural alignment. Enums use the same `{ i8 tag +
/// align padding + max-payload }` blob the LLVM enum layout
/// produces, with the outer rounded up to its max-align stride.
/// A union resolves through `unions`. Before the sizing pass
/// reaches its decl, the payload size is 0, a lower bound that
/// [`refine_nested_union_sizes`] settles. Panics when `unions` has
/// no decl for the union.
pub(crate) fn size_in_bytes(
    ty: &IRType,
    declarations: &Declarations<'_>,
    unions: &BTreeMap<IRSymbol, IRUnionDecl>,
) -> u32 {
    let (size, _align) = size_and_align(ty, declarations, unions);
    size
}

/// `(size, align)` for an [`IRType`] using the same ABI rules
/// `size_in_bytes` documents. Internal helper exported so the
/// struct / enum walkers can apply field padding correctly.
fn size_and_align(
    ty: &IRType,
    declarations: &Declarations<'_>,
    unions: &BTreeMap<IRSymbol, IRUnionDecl>,
) -> (u32, u32) {
    const PTR_BYTES: u32 = 8;
    match ty {
        IRType::Bool | IRType::Int8 | IRType::UInt8 => (1, 1),
        IRType::Int16 | IRType::UInt16 => (2, 2),
        IRType::Float32 | IRType::Int32 | IRType::UInt32 => (4, 4),
        IRType::Float64 | IRType::Int64 | IRType::UInt64 => (8, 8),
        IRType::Unit => (1, 1),
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
        IRType::Struct(symbol) => match declarations.struct_decl(symbol) {
            Some(decl) => sum_fields(decl.fields.iter().map(|f| &f.ir_type), declarations, unions),
            None => (PTR_BYTES, PTR_BYTES),
        },
        IRType::Tuple(elements) => sum_fields(elements.iter(), declarations, unions),
        IRType::Enum(symbol) => match declarations.enum_decl(symbol) {
            Some(decl) => enum_size(decl, declarations, unions),
            None => (PTR_BYTES, PTR_BYTES),
        },
        // A union sizes through its declaration only. A walk into
        // its members could reach the union again. Before the sizing
        // pass reaches the declaration, the size is a lower bound
        // that `refine_nested_union_sizes` settles. Lowering declares
        // each union when a type first names it, so a union with no
        // declaration is a compiler bug.
        IRType::Union(symbol) => match unions.get(symbol) {
            Some(decl) => union_size(decl.max_payload_size),
            None => panic!("IR: union `{symbol}` has no declaration"),
        },
    }
}

/// Sum a sequence of field types into `(size, align)` accounting
/// for natural alignment padding and trailing pad-to-align.
/// Mirrors the layout `inkwell` would produce for a
/// `context.struct_type(&[...], false)` (non-packed) call.
fn sum_fields<'a, I>(
    fields: I,
    declarations: &Declarations<'_>,
    unions: &BTreeMap<IRSymbol, IRUnionDecl>,
) -> (u32, u32)
where
    I: IntoIterator<Item = &'a IRType>,
{
    let mut size = 0u32;
    let mut align = 1u32;
    for field in fields {
        let (field_size, field_align) = size_and_align(field, declarations, unions);
        size = round_up(size, field_align);
        size += field_size;
        align = align.max(field_align);
    }
    size = round_up(size, align);
    (size, align)
}

/// Enum payload sizing mirrors `koja-ir-llvm`'s layout. Each
/// variant is a `{ i8 tag, [pad x i8], payload }` blob, and the outer
/// is `{ [count x iN] }` where `N = max_align * 8` and the byte
/// count is `count * max_align >= max_complete_size`. Returns the
/// outer's `(size, align)`.
fn enum_size(
    decl: &IREnumDecl,
    declarations: &Declarations<'_>,
    unions: &BTreeMap<IRSymbol, IRUnionDecl>,
) -> (u32, u32) {
    let mut max_complete_size = 0u32;
    let mut max_complete_align = 1u32;
    for variant in &decl.variants {
        let (payload_size, payload_align) = match &variant.payload {
            // A unit variant has no value-position payload. Its
            // complete representation is only the tag byte.
            IRVariantPayload::Unit => (0, 1),
            IRVariantPayload::Tuple(types) => sum_fields(types.iter(), declarations, unions),
            IRVariantPayload::Struct(fields) => {
                sum_fields(fields.iter().map(|f| &f.ir_type), declarations, unions)
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

/// `(size, align)` of a union outer holding `payload` bytes: one
/// `i64` tag word plus the payload rounded up to whole words,
/// mirroring the `{ i64, [M x i64] }` LLVM shape.
fn union_size(payload: u32) -> (u32, u32) {
    (8 + 8 * payload.div_ceil(8), 8)
}

/// Set `max_payload_size` on every union declaration in `packages`.
/// Lowering declares each union with no payload size, in every
/// package that names it. The copies of one declaration get the same
/// size. The type cycles must be broken first, or the size of a
/// struct that holds its own union has no end.
pub(crate) fn size_unions(packages: &mut [IRPackage]) {
    let declarations = Declarations::new(&*packages);
    let mut sized: Vec<BTreeMap<IRSymbol, IRUnionDecl>> =
        packages.iter().map(|pkg| pkg.unions.clone()).collect();
    refine_nested_union_sizes(&mut sized, &declarations);
    for (pkg, unions) in packages.iter_mut().zip(sized) {
        pkg.unions = unions;
    }
}

/// Compute `max_payload_size` for every declaration in `staged`.
/// Iterates until fixpoint, so a union that holds another union
/// settles into a stable byte count after the inner one does.
fn refine_nested_union_sizes(
    staged: &mut [BTreeMap<IRSymbol, IRUnionDecl>],
    declarations: &Declarations<'_>,
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
                    .map(|m| size_in_bytes(m, declarations, &snapshot))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn size(ty: &IRType) -> u32 {
        size_in_bytes(ty, &Declarations::new(&[]), &BTreeMap::new())
    }

    #[test]
    fn value_position_unit_matches_llvm_i8_placeholder() {
        assert_eq!(size(&IRType::Unit), 1);
    }

    #[test]
    fn tuples_with_unit_include_natural_padding() {
        assert_eq!(size(&IRType::Tuple(vec![IRType::Unit, IRType::Int64])), 16);
        assert_eq!(size(&IRType::Tuple(vec![IRType::Int64, IRType::Unit])), 16);
        assert_eq!(size(&IRType::Tuple(vec![IRType::Unit, IRType::Unit])), 2);
    }

    fn union_type(name: &str) -> IRType {
        IRType::Union(IRSymbol::synthetic(name.to_string()))
    }

    fn unsized_decl(name: &str, members: Vec<IRType>) -> IRUnionDecl {
        IRUnionDecl {
            max_payload_size: 0,
            members,
            symbol: IRSymbol::synthetic(name.to_string()),
        }
    }

    fn package_with(name: &str, unions: Vec<IRUnionDecl>) -> IRPackage {
        IRPackage {
            constants: BTreeMap::new(),
            enums: BTreeMap::new(),
            functions: BTreeMap::new(),
            package: name.to_string(),
            structs: BTreeMap::new(),
            unions: unions
                .into_iter()
                .map(|decl| (decl.symbol.clone(), decl))
                .collect(),
        }
    }

    /// The size of the union `name` after the sizing pass, read
    /// through the declarations of the first package.
    fn sized(packages: &[IRPackage], name: &str) -> u32 {
        size_in_bytes(
            &union_type(name),
            &Declarations::new(packages),
            &packages[0].unions,
        )
    }

    #[test]
    fn unions_are_a_tag_word_plus_word_chunked_payload() {
        let tuple = IRType::Tuple(vec![IRType::Unit, IRType::Int64]);
        let mut packages = vec![package_with(
            "TestApp",
            vec![
                unsized_decl("U_bool", vec![IRType::Bool]),
                unsized_decl("U_tuple", vec![tuple]),
            ],
        )];
        size_unions(&mut packages);
        // A 1-byte payload rounds up to one word: { i64, [1 x i64] }.
        assert_eq!(sized(&packages, "U_bool"), 16);
        // A 16-byte tuple payload is two words: { i64, [2 x i64] }.
        assert_eq!(sized(&packages, "U_tuple"), 24);
    }

    #[test]
    fn nested_unions_size_through_the_outer_shape() {
        let mut packages = vec![package_with(
            "TestApp",
            vec![
                unsized_decl("U_inner", vec![IRType::Int64]),
                unsized_decl("U_outer", vec![union_type("U_inner")]),
            ],
        )];
        size_unions(&mut packages);
        // Inner { i64, [1 x i64] } = 16 bytes becomes the outer's
        // payload: { i64, [2 x i64] } = 24 bytes.
        assert_eq!(sized(&packages, "U_outer"), 24);
    }

    #[test]
    #[should_panic(expected = "union `U_missing` has no declaration")]
    fn a_union_with_no_declaration_panics() {
        size(&union_type("U_missing"));
    }

    #[test]
    #[should_panic(expected = "union `U_missing` has no declaration")]
    fn a_member_that_names_a_union_with_no_declaration_panics() {
        let mut packages = vec![package_with(
            "TestApp",
            vec![unsized_decl("U_outer", vec![union_type("U_missing")])],
        )];
        size_unions(&mut packages);
    }

    #[test]
    fn copies_of_one_declaration_get_the_same_size() {
        let decl = unsized_decl("U_shared", vec![IRType::Int64, IRType::String]);
        let mut packages = vec![
            package_with("First", vec![decl.clone()]),
            package_with("Second", vec![decl]),
        ];
        size_unions(&mut packages);
        let symbol = IRSymbol::synthetic("U_shared".to_string());
        assert_eq!(packages[0].unions[&symbol].max_payload_size, 8);
        assert_eq!(packages[0].unions[&symbol], packages[1].unions[&symbol]);
    }
}
