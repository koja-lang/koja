//! Lowering for `match` arms and the patterns within them.
//!
//! Walks the AST patterns alongside the subject's resolved type, picks the
//! right enum tag / struct field layout / tuple element decomposition,
//! and produces [`crate::resolved::patterns`] / [`crate::resolved::match_expr`]
//! values that the match-emission scaffolding can consume mechanically.

use expo_ast::ast::{Expr, ExprKind, FieldPattern, Literal, MatchArm, Pattern};
use expo_ast::identifier::{Package, TypeIdentifier};
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{Type, mangle_name, mangle_type, named, unwrap_indirect};

use crate::Lowerer;
use crate::blocks::{IRBasicBlock, IRBlockId, IRTerminator};
use crate::cfg::CFGBuilder;
use crate::identity::MonomorphizedTypeIdentifier;
use crate::lower::ctx::LowerCtx;
use crate::lower::mangling::try_parse_mangled_name;
use crate::lower::types::{
    find_type_current, monomorphize_type, resolve_name_current, resolve_type_expr,
};
use crate::resolved::match_expr::ResolvedMatchType;
use crate::resolved::ops::ResolvedBinaryOp;
use crate::resolved::patterns::{ResolvedFieldPattern, ResolvedLiteral, ResolvedPattern};
use crate::util::parse_int_literal;
use crate::values::{IRInstruction, IROperand};

/// Picks the most specific Expo type available for the match subject.
/// Prefers the post-emit `expo_type` (codegen has full monomorphization
/// context, even inside generic impl bodies whose typechecked
/// `resolved_type` doesn't reach the cached AST clones); falls back to the
/// typecheck-populated `resolved_type` and finally to the variable-binding
/// heuristic. Always returns a usable type when any source has one; only
/// returns `Type::Unknown` when every source agrees there is none.
///
/// `var_type` looks a binding name up in the surrounding LLVM-bound
/// variables map (which expo-ir cannot reach into directly because that
/// map's value carries `BasicValueEnum<'ctx>`).
pub fn resolve_subject_ty(
    ctx: &LowerCtx<'_>,
    subject: &Expr,
    post_emit_ty: &Type,
    var_type: impl Fn(&str) -> Option<Type>,
) -> Type {
    if !matches!(post_emit_ty, Type::Unknown) {
        return post_emit_ty.clone();
    }
    if let Some(ty) = subject.resolved_type.as_ref() {
        let substituted = monomorphize_type(ctx, ty);
        if !matches!(substituted, Type::Unknown) {
            return substituted;
        }
    }
    if let ExprKind::Ident { name, .. } = &subject.kind
        && let Some(ty) = var_type(name)
    {
        return ty;
    }
    if matches!(subject.kind, ExprKind::Self_)
        && let Some(ty) = var_type("self")
    {
        return ty;
    }
    Type::Unknown
}

/// Resolves an AST pattern against the subject's Expo type, producing a
/// `ResolvedPattern` whose enum keys, tags, field indices, and variant
/// shapes have all been validated against the type registry.
pub fn lower_pattern(
    ctx: &LowerCtx<'_>,
    pattern: &Pattern,
    subject_type: &Type,
) -> Result<ResolvedPattern, String> {
    match pattern {
        Pattern::Wildcard { .. } => Ok(ResolvedPattern::AlwaysMatch),

        Pattern::Binding { name, .. } => Ok(ResolvedPattern::Bind {
            name: name.clone(),
            ty: subject_type.clone(),
            strict_llvm: false,
        }),

        Pattern::Literal { value, .. } => Ok(ResolvedPattern::LiteralEq {
            lit: lower_literal(value)?,
            subject_ty: subject_type.clone(),
        }),

        Pattern::EnumUnit {
            type_path, variant, ..
        } => {
            let enum_key = resolve_enum_key_from_path(ctx, type_path, subject_type)?;
            let tag = lookup_variant_tag(ctx, &enum_key, variant)?;
            Ok(ResolvedPattern::EnumUnit {
                enum_key,
                variant: variant.clone(),
                tag,
            })
        }

        Pattern::EnumTuple {
            type_path,
            variant,
            elements,
            ..
        } => {
            let enum_key = resolve_enum_key_from_path(ctx, type_path, subject_type)?;
            let tag = lookup_variant_tag(ctx, &enum_key, variant)?;
            let elements = lower_tuple_elements(ctx, &enum_key, variant, elements)?;
            Ok(ResolvedPattern::EnumTuple {
                enum_key,
                variant: variant.clone(),
                tag,
                elements,
            })
        }

        Pattern::EnumStruct {
            type_path,
            variant,
            fields,
            ..
        } => {
            let enum_key = resolve_enum_key_from_path(ctx, type_path, subject_type)?;
            let tag = lookup_variant_tag(ctx, &enum_key, variant)?;
            let fields = lower_struct_fields(ctx, &enum_key, variant, fields)?;
            Ok(ResolvedPattern::EnumStruct {
                enum_key,
                variant: variant.clone(),
                tag,
                fields,
            })
        }

        Pattern::Struct {
            type_path, fields, ..
        } => {
            let struct_key = resolve_struct_key_from_path(ctx, type_path, subject_type)?;
            let expected = get_struct_fields(ctx, &struct_key)?;
            let resolved =
                lower_field_patterns(ctx, &expected, fields, &format!("struct `{struct_key}`"))?;
            Ok(ResolvedPattern::Struct {
                struct_key,
                fields: resolved,
            })
        }

        Pattern::Constructor { name, elements, .. } => {
            let enum_key = resolve_enum_key_from_constructor(ctx, name, subject_type)?;
            let tag = lookup_variant_tag(ctx, &enum_key, name)?;
            if elements.is_empty() {
                // Constructor with no payload acts as a unit-variant tag check
                // -- collapsing to `EnumUnit` keeps emission's no-payload-GEP
                // invariant uniform.
                Ok(ResolvedPattern::EnumUnit {
                    enum_key,
                    variant: name.clone(),
                    tag,
                })
            } else {
                let elements = lower_tuple_elements(ctx, &enum_key, name, elements)?;
                Ok(ResolvedPattern::EnumTuple {
                    enum_key,
                    variant: name.clone(),
                    tag,
                    elements,
                })
            }
        }

        Pattern::TypedBinding {
            name, type_expr, ..
        } => {
            let resolved = resolve_type_expr(ctx, type_expr);
            let subject_inner = unwrap_indirect(subject_type);

            if mangle_type(&resolved) == mangle_type(subject_inner) {
                Ok(ResolvedPattern::Bind {
                    name: name.clone(),
                    ty: resolved,
                    strict_llvm: true,
                })
            } else {
                let union_mangled = mangle_type(subject_inner);
                let member_mangled = mangle_type(&resolved);
                let tag = union_member_tag(subject_inner, &member_mangled).ok_or_else(|| {
                    format!("unknown union member: {union_mangled}.{member_mangled}")
                })?;
                Ok(ResolvedPattern::UnionMember {
                    union_mangled: MonomorphizedTypeIdentifier::new(union_mangled),
                    member_mangled: MonomorphizedTypeIdentifier::new(member_mangled),
                    tag,
                    member_ty: resolved,
                    bind_name: name.clone(),
                })
            }
        }

        Pattern::List { .. } => Err("list patterns not yet supported in compilation".to_string()),

        Pattern::Binary { segments, .. } => Ok(ResolvedPattern::Binary {
            segments: segments.clone(),
        }),

        Pattern::Or { patterns, .. } => {
            let mut subs = Vec::with_capacity(patterns.len());
            for p in patterns {
                subs.push(lower_pattern(ctx, p, subject_type)?);
            }
            Ok(ResolvedPattern::Or(subs))
        }
    }
}

pub fn lower_literal(lit: &Literal) -> Result<ResolvedLiteral, String> {
    match lit {
        Literal::Bool(b) => Ok(ResolvedLiteral::Bool(*b)),
        Literal::Float(s) => s
            .parse::<f64>()
            .map(ResolvedLiteral::Float)
            .map_err(|_| format!("invalid float: {s}")),
        Literal::Int(s) => parse_int_literal(s).map(ResolvedLiteral::Int),
        Literal::String(s) => Ok(ResolvedLiteral::String(s.clone())),
        Literal::Unit => Err("unsupported literal in match pattern".to_string()),
    }
}

pub fn lower_tuple_elements(
    ctx: &LowerCtx<'_>,
    enum_key: &str,
    variant: &str,
    elements: &[Pattern],
) -> Result<Vec<(Type, ResolvedPattern)>, String> {
    let field_types = get_tuple_variant_types(ctx, enum_key, variant)?;
    if elements.len() != field_types.len() {
        return Err(format!(
            "variant {enum_key}.{variant} expects {} payload elements, got {}",
            field_types.len(),
            elements.len()
        ));
    }
    let mut out = Vec::with_capacity(elements.len());
    for (sub, ft) in elements.iter().zip(field_types.iter()) {
        let inner = unwrap_indirect(ft).clone();
        let sub_resolved = lower_pattern(ctx, sub, &inner)?;
        out.push((ft.clone(), sub_resolved));
    }
    Ok(out)
}

pub fn lower_struct_fields(
    ctx: &LowerCtx<'_>,
    enum_key: &str,
    variant: &str,
    fields: &[FieldPattern],
) -> Result<Vec<ResolvedFieldPattern>, String> {
    let expected = get_struct_variant_fields(ctx, enum_key, variant)?;
    lower_field_patterns(ctx, &expected, fields, &format!("{enum_key}.{variant}"))
}

/// Resolve a list of `FieldPattern`s against a known field layout. Shared
/// by [`lower_struct_fields`] (enum-struct variant context) and by the
/// plain `Pattern::Struct` arm in [`lower_pattern`]. `container_label`
/// is the diagnostic prefix used when an unknown field is referenced
/// (e.g. `"Color.Red"` or `"struct \`Point\`"`).
pub fn lower_field_patterns(
    ctx: &LowerCtx<'_>,
    expected_fields: &[(String, Type)],
    fields: &[FieldPattern],
    container_label: &str,
) -> Result<Vec<ResolvedFieldPattern>, String> {
    let mut out = Vec::with_capacity(fields.len());
    for fp in fields {
        let (idx, (_, field_type)) = expected_fields
            .iter()
            .enumerate()
            .find(|(_, (n, _))| *n == fp.name)
            .ok_or_else(|| format!("unknown field `{}` in {container_label}", fp.name))?;
        let inner_ty = unwrap_indirect(field_type);
        out.push(ResolvedFieldPattern {
            name: fp.name.clone(),
            field_index: idx as u32,
            field_type: field_type.clone(),
            sub: lower_pattern(ctx, &fp.pattern, inner_ty)?,
        });
    }
    Ok(out)
}

/// Resolves the canonical type-cache key for an enum referenced via an AST
/// type path (`Color`, `alpha.Status`, generic-args-bearing `Option<T>`
/// already monomorphized in the subject type, etc.).
pub fn resolve_enum_key_from_path(
    ctx: &LowerCtx<'_>,
    type_path: &[String],
    subject_type: &Type,
) -> Result<String, String> {
    let ty = unwrap_indirect(subject_type);
    match ty {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => Ok(mangle_name(identifier, type_args)),
        Type::Named { identifier, .. } => {
            let name = &identifier.name;
            if let Some((base, _)) = try_parse_mangled_name(ctx, name)
                && ctx.type_ctx.is_enum(&base)
            {
                Ok(name.clone())
            } else if identifier.package != Package::Unresolved {
                Ok(identifier.qualified_name())
            } else if !type_path.is_empty() {
                resolve_enum_key_from_joined(ctx, &type_path.join("."), subject_type)
            } else {
                Err("cannot determine enum name for pattern".to_string())
            }
        }
        _ if !type_path.is_empty() => {
            resolve_enum_key_from_joined(ctx, &type_path.join("."), subject_type)
        }
        _ => Err("cannot determine enum name for pattern".to_string()),
    }
}

/// Resolves the canonical type-cache key for a shorthand constructor
/// pattern (`Some(x)`, `Ok(_)`) -- where the variant name is given without
/// an enum-name qualifier.
pub fn resolve_enum_key_from_constructor(
    ctx: &LowerCtx<'_>,
    variant_name: &str,
    subject_type: &Type,
) -> Result<String, String> {
    let subject_type = unwrap_indirect(subject_type);
    if let Type::Named {
        identifier,
        type_args,
    } = subject_type
    {
        let name = &identifier.name;
        if !type_args.is_empty() {
            return Ok(mangle_name(identifier, type_args));
        }
        if let Some((base, _)) = try_parse_mangled_name(ctx, name)
            && ctx.type_ctx.is_enum(&base)
        {
            return Ok(name.clone());
        }
        if identifier.package != Package::Unresolved
            && ctx
                .type_ctx
                .get_type(identifier)
                .is_some_and(|ti| ti.is_enum())
        {
            return Ok(identifier.qualified_name());
        }
        if ctx.type_ctx.is_enum(name) {
            return Ok(name.clone());
        }
    }
    if let Type::Union(members) = subject_type {
        let member_mangled = mangle_type(&named(variant_name));
        if members.iter().any(|m| mangle_type(m) == member_mangled) {
            return Ok(mangle_type(subject_type));
        }
    }
    for (enum_name, info) in ctx.type_ctx.types.iter().filter(|(_, ti)| ti.is_enum()) {
        if info
            .variants()
            .is_some_and(|vs| vs.iter().any(|v| v.name == variant_name))
        {
            return Ok(enum_name.name.clone());
        }
    }
    Err(format!("no enum found with variant `{variant_name}`"))
}

fn resolve_enum_key_from_joined(
    ctx: &LowerCtx<'_>,
    joined: &str,
    subject_type: &Type,
) -> Result<String, String> {
    if let Some(id) = resolve_name_current(ctx, joined) {
        let qualified = id.qualified_name();
        let qualified_id = MonomorphizedTypeIdentifier::new(&qualified);
        if ctx.type_ctx.get_type(id).is_some()
            || ctx.layouts.contains_monomorphized(&qualified_id)
            || ctx.layouts.contains_enum(&qualified_id)
        {
            return Ok(qualified);
        }
    }
    let bare_id = TypeIdentifier::from_qualified_name(joined);
    let joined_id = MonomorphizedTypeIdentifier::new(joined);
    if ctx.type_ctx.get_type(&bare_id).is_some()
        || ctx.layouts.contains_monomorphized(&joined_id)
        || ctx.layouts.contains_enum(&joined_id)
    {
        return Ok(joined.to_string());
    }
    Err(format!(
        "cannot resolve enum name from pattern `{joined}` for match subject type `{}`",
        subject_type.display()
    ))
}

fn lookup_variant_tag(ctx: &LowerCtx<'_>, enum_key: &str, variant: &str) -> Result<u8, String> {
    ctx.layouts
        .variant_index(&MonomorphizedTypeIdentifier::new(enum_key), variant)
        .ok_or_else(|| format!("unknown variant: {enum_key}.{variant}"))
}

/// Tag (= position) of a union member, derived directly from the union's
/// member list. Unions do not flow through `TypeLayouts` or `LLVMTypeCache`
/// — their tag and payload are fully determined by the surrounding
/// `Type::Union(members)` at the use site.
fn union_member_tag(union_ty: &Type, member_mangled: &str) -> Option<u8> {
    let Type::Union(members) = union_ty else {
        return None;
    };
    members
        .iter()
        .position(|m| mangle_type(m) == member_mangled)
        .map(|i| i as u8)
}

fn get_struct_variant_fields(
    ctx: &LowerCtx<'_>,
    enum_key: &str,
    variant: &str,
) -> Result<Vec<(String, Type)>, String> {
    let data = lookup_variant_data(ctx, enum_key, variant)?;
    match data {
        VariantData::Struct(fields) => Ok(fields),
        _ => Err(format!("{enum_key}.{variant} is not a struct variant")),
    }
}

/// Resolves the LLVMTypeCache key for a plain struct referenced via an
/// AST type path. Mirrors [`resolve_enum_key_from_path`] but resolves
/// against struct registrations: applies generic-arg mangling when the
/// subject's resolved type carries them, otherwise prefers the
/// package-qualified identifier and falls back to a name-only lookup.
pub fn resolve_struct_key_from_path(
    ctx: &LowerCtx<'_>,
    type_path: &[String],
    subject_type: &Type,
) -> Result<String, String> {
    let ty = unwrap_indirect(subject_type);
    match ty {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => Ok(mangle_name(identifier, type_args)),
        Type::Named { identifier, .. } => {
            let name = &identifier.name;
            if let Some((base, _)) = try_parse_mangled_name(ctx, name)
                && ctx.type_ctx.is_struct(&base)
            {
                Ok(name.clone())
            } else if identifier.package != Package::Unresolved {
                Ok(identifier.qualified_name())
            } else if !type_path.is_empty() {
                resolve_struct_key_from_joined(ctx, &type_path.join("."), subject_type)
            } else {
                Err("cannot determine struct name for pattern".to_string())
            }
        }
        _ if !type_path.is_empty() => {
            resolve_struct_key_from_joined(ctx, &type_path.join("."), subject_type)
        }
        _ => Err("cannot determine struct name for pattern".to_string()),
    }
}

fn resolve_struct_key_from_joined(
    ctx: &LowerCtx<'_>,
    joined: &str,
    subject_type: &Type,
) -> Result<String, String> {
    if let Some(id) = resolve_name_current(ctx, joined) {
        let qualified = id.qualified_name();
        let qualified_id = MonomorphizedTypeIdentifier::new(&qualified);
        if ctx.type_ctx.get_type(id).is_some() || ctx.layouts.contains_monomorphized(&qualified_id)
        {
            return Ok(qualified);
        }
    }
    let bare_id = TypeIdentifier::from_qualified_name(joined);
    let joined_id = MonomorphizedTypeIdentifier::new(joined);
    if ctx.type_ctx.get_type(&bare_id).is_some() || ctx.layouts.contains_monomorphized(&joined_id) {
        return Ok(joined.to_string());
    }
    Err(format!(
        "cannot resolve struct name from pattern `{joined}` for match subject type `{}`",
        subject_type.display()
    ))
}

/// Field-name/type pairs for a struct identified by its cache key. Tries
/// the in-context registry first (for non-generic structs and generic
/// structs whose type-params are already substituted at the type-info
/// level) and falls back to a parsed-mangled-name lookup for monomorphic
/// instances of generic structs.
fn get_struct_fields(ctx: &LowerCtx<'_>, struct_key: &str) -> Result<Vec<(String, Type)>, String> {
    if let Some(ti) = find_type_current(ctx, struct_key)
        && let Some(fields) = ti.fields()
    {
        return Ok(fields.clone());
    }
    if let Some((base, type_args)) = try_parse_mangled_name(ctx, struct_key)
        && let Some(ti) = find_type_current(ctx, &base)
        && let Some(fields) = ti.fields()
    {
        if ti.type_params.is_empty() || type_args.is_empty() {
            return Ok(fields.clone());
        }
        let subst = expo_typecheck::types::build_substitution(&ti.type_params, &type_args);
        return Ok(fields
            .iter()
            .map(|(n, t)| {
                (
                    n.clone(),
                    expo_typecheck::types::substitute_preserving(t, &subst),
                )
            })
            .collect());
    }
    Err(format!("struct fields not found for `{struct_key}`"))
}

fn get_tuple_variant_types(
    ctx: &LowerCtx<'_>,
    enum_key: &str,
    variant: &str,
) -> Result<Vec<Type>, String> {
    let data = lookup_variant_data(ctx, enum_key, variant)?;
    match data {
        VariantData::Tuple(types) => Ok(types),
        _ => Err(format!("{enum_key}.{variant} is not a tuple variant")),
    }
}

fn lookup_variant_data(
    ctx: &LowerCtx<'_>,
    enum_name: &str,
    variant: &str,
) -> Result<VariantData, String> {
    if let Some(ti) = find_type_current(ctx, enum_name)
        && let Some(vs) = ti.variants()
        && let Some(vi) = vs.iter().find(|v| v.name == variant)
    {
        return Ok(vi.data.clone());
    }
    if let Some(variants) = ctx
        .layouts
        .enum_variants(&MonomorphizedTypeIdentifier::new(enum_name))
        && let Some((_, data)) = variants.iter().find(|(n, _)| n == variant)
    {
        return Ok(data.clone());
    }
    Err(format!("variant not found: {enum_name}.{variant}"))
}

/// Per-arm + global block / value identifiers minted for the
/// `match` lowering. Pulled out into a struct so
/// [`Lowerer::lower_match_expr`] can hand the ids to the per-arm
/// builder without threading them as a long parameter list.
struct MatchBlockIds {
    arm_body_blocks: Vec<IRBlockId>,
    arm_check_blocks: Vec<IRBlockId>,
    fallthrough_block: IRBlockId,
    merge_block: IRBlockId,
}

/// Per-arm intermediate emitted during the `match` lowering. The
/// pattern arm's check sub-CFG is built into a temporary
/// `Vec<IRBasicBlock>` (via the gated CFG builder
/// [`Lowerer::lower_pattern_into_arm`]); the body block is built
/// alongside. Both are drained into the [`crate::CFGBuilder`] in
/// source order by [`Lowerer::lower_match_expr`].
struct LoweredMatchArm {
    body: IRBasicBlock,
    check_blocks: Vec<IRBasicBlock>,
    trailing_value: Option<IROperand>,
}

/// Outcome of lowering a single AST [`Pattern`] into a flat
/// instruction stream + final i1 operand. The single-pattern entry
/// point ([`Lowerer::lower_pattern_to_instructions`], used by
/// [`compile_pattern`] for `receive` arms and `expr matches Pattern`)
/// produces this shape.
///
/// `instructions` is a single ordered stream containing both the
/// pattern-test primitives ([`IRInstruction::PatternTagEq`] /
/// [`IRInstruction::PatternLiteralEq`] /
/// [`IRInstruction::PatternProjectVariantField`] /
/// [`IRInstruction::PatternUnionPayloadPtr`] /
/// [`IRInstruction::PatternBinaryMatch`]) and the binding-setup
/// instructions ([`IRInstruction::PatternBindFromPtr`]), interleaved
/// in source order. AND/OR fusion of i1 results uses
/// [`IRInstruction::BinaryOp`] (`BoolAnd` / `BoolOr`).
///
/// `check_result` is the [`IROperand`] consumers of the lowered
/// pattern reference for the resulting `IntValue`
/// ([`compile_pattern`] for `receive` arms and `matches`).
///
/// **Match arms do not use this shape.** They route through
/// [`Lowerer::lower_pattern_into_arm`] (the gated CFG builder) so
/// payload-projection instructions only execute on the success branch
/// of their enclosing tag check. Keeping the flat shape for the
/// single-pattern path is intentional for now: `receive` and `matches`
/// don't yet have a CFG-style emission surface, so they continue to
/// emit the pattern as a single linear stream at the current builder
/// position. Lifting them to the gated shape is a separate follow-up.
pub struct LoweredPattern {
    pub check_result: IROperand,
    pub instructions: Vec<IRInstruction>,
}

impl<'a> Lowerer<'a> {
    /// Lowers a `match` expression into an [`IRMatch`].
    ///
    /// Pattern resolution flows through [`lower_pattern`] (per arm) and
    /// then [`Self::lower_pattern_into_arm`] (the gated CFG builder) to
    /// produce each arm's `check_blocks` sub-CFG. `result_ty` is the
    /// typecheck-derived Direct vs UnionWrap strategy, computed up-front
    /// by the codegen shim and threaded in.
    ///
    /// Guards lift to operand-emitting instructions appended to the
    /// final block of the arm's check sub-CFG; the result operand is
    /// `BoolAnd`-fused with the pattern's final i1 to produce the cond
    /// for the body branch. No codegen-side guard handling remains.
    ///
    /// The arm chain: failure edges from any block in
    /// `arms[i].check_blocks` target `arms[i+1].check_blocks[0].id` for
    /// `i < N-1`, and `fallthrough_block` for the last arm.
    /// `fallthrough_block` is the all-patterns-failed landing pad
    /// (matches legacy semantics); the inline-synthesized merge phi
    /// registers an `undef` incoming from it when value-producing.
    pub fn lower_match_expr(
        &mut self,
        builder: &mut CFGBuilder,
        open: IRBlockId,
        subject_ptr: IROperand,
        subject_ty: Type,
        arms: &[MatchArm],
        result_ty: ResolvedMatchType,
    ) -> Result<(Option<IRBlockId>, IROperand), String> {
        let ids = self.mint_match_block_ids(arms.len());

        // Branch from caller's open block into the first arm's check entry.
        builder.set_terminator(open, IRTerminator::Branch(ids.arm_check_blocks[0]));

        let mut lowered_arms = Vec::with_capacity(arms.len());
        for (i, arm) in arms.iter().enumerate() {
            lowered_arms.push(self.build_match_arm(
                arm,
                &subject_ty,
                &subject_ptr,
                &ids,
                i,
                &result_ty,
            )?);
        }

        // Drain per-arm check sub-CFGs and body blocks into the builder.
        for arm in &lowered_arms {
            for blk in &arm.check_blocks {
                builder.add_block(blk.id, blk.label.clone());
                for instr in &blk.instructions {
                    builder.append(blk.id, instr.clone());
                }
                builder.set_terminator(blk.id, blk.terminator.clone());
            }
            builder.add_block(arm.body.id, arm.body.label.clone());
            for instr in &arm.body.instructions {
                builder.append(arm.body.id, instr.clone());
            }
            builder.set_terminator(arm.body.id, arm.body.terminator.clone());
        }

        // Fallthrough block: all-patterns-failed landing pad. Branches
        // into merge so the merge phi can register an `undef` incoming
        // for it (legacy semantics).
        builder.add_block(ids.fallthrough_block, "match_none");
        builder.set_terminator(ids.fallthrough_block, IRTerminator::Branch(ids.merge_block));

        builder.add_block(ids.merge_block, "match_end");

        // Pre-stage merge phi if every reachable arm produced a value.
        let result = self.try_stage_match_phi(builder, &ids, &lowered_arms, &result_ty);
        Ok((Some(ids.merge_block), result))
    }

    /// Pre-stage the merge [`IRInstruction::Phi`] when every
    /// non-self-terminated arm produced a trailing value AND no arm
    /// reached `Branch(merge)` without one. Returns
    /// [`IROperand::Local`] referencing the phi's dest, or
    /// [`IROperand::Unit`] for statement-shaped merges. Mirrors legacy
    /// `assemble_match_phi`'s all-or-nothing contract.
    ///
    /// The merge block has one predecessor per value-producing arm
    /// plus the `fallthrough_block` (always present as an
    /// all-patterns-failed landing pad). The Phi includes an
    /// `IROperand::Unit` sentinel for the fallthrough; the codegen
    /// executor materializes it as `undef` of the same LLVM type as
    /// the other incomings (legacy parity).
    fn try_stage_match_phi(
        &mut self,
        builder: &mut CFGBuilder,
        ids: &MatchBlockIds,
        lowered_arms: &[LoweredMatchArm],
        result_ty: &ResolvedMatchType,
    ) -> IROperand {
        let mut incomings: Vec<(IRBlockId, IROperand)> = Vec::new();
        let mut any_no_value_branch = false;
        for arm in lowered_arms {
            match (&arm.body.terminator, &arm.trailing_value) {
                (IRTerminator::Branch(_), Some(op)) => {
                    incomings.push((arm.body.id, op.clone()));
                }
                (IRTerminator::Branch(_), None) => any_no_value_branch = true,
                _ => {} // self-terminated arm (Return / Unreachable / pre-staged CondBranch)
            }
        }
        if incomings.is_empty() || any_no_value_branch {
            return IROperand::Unit;
        }
        // Fallthrough block branches to merge unconditionally; the
        // Phi must include an entry for it. Use IROperand::Unit as
        // the sentinel; the codegen executor materializes it as
        // `undef` of the matching LLVM type.
        incomings.push((ids.fallthrough_block, IROperand::Unit));
        let ty = match result_ty {
            ResolvedMatchType::Direct { ty } => ty.clone(),
            ResolvedMatchType::UnionWrap { target } => target.clone(),
        };
        let dest = self.next_value_id();
        builder.append(
            ids.merge_block,
            IRInstruction::Phi {
                dest,
                incomings,
                ty,
            },
        );
        IROperand::Local(dest)
    }

    /// Lowers a single AST [`Pattern`] (with its subject's resolved
    /// `Type` and an [`IROperand`] referencing the subject's storage)
    /// into a [`LoweredPattern`]. Shared lift surface used by both
    /// [`Self::lower_match_expr`] (per arm) and `compile_pattern`
    /// (single-pattern entry from `receive` arms /
    /// `ExprKind::PatternMatch`).
    pub fn lower_pattern_to_instructions(
        &mut self,
        pattern: &Pattern,
        subject_ty: &Type,
        subject_ptr: IROperand,
    ) -> Result<LoweredPattern, String> {
        let resolved = lower_pattern(&self.ctx(), pattern, subject_ty)?;
        let mut instructions = Vec::new();
        let result = self.lower_resolved_pattern(&resolved, &subject_ptr, &mut instructions)?;
        Ok(LoweredPattern {
            check_result: result,
            instructions,
        })
    }

    /// Mints fresh per-arm and global identifiers for the `match`
    /// lowering: one `check_block` and one `body_block` per arm, plus
    /// shared `fallthrough_block` and `merge_block`. The subject
    /// pointer comes in as an [`IROperand`] from the caller (typically
    /// the codegen shim passing the LLVM alloca's pointer through
    /// the value map).
    fn mint_match_block_ids(&mut self, arm_count: usize) -> MatchBlockIds {
        let arm_check_blocks: Vec<_> = (0..arm_count).map(|_| self.next_block_id()).collect();
        let arm_body_blocks: Vec<_> = (0..arm_count).map(|_| self.next_block_id()).collect();
        let fallthrough_block = self.next_block_id();
        let merge_block = self.next_block_id();
        MatchBlockIds {
            arm_body_blocks,
            arm_check_blocks,
            fallthrough_block,
            merge_block,
        }
    }

    /// Builds a single [`IRMatchArm`]. Lowers the arm's pattern via
    /// [`Self::lower_pattern_into_arm`] (which writes one or more
    /// blocks into the arm's check sub-CFG, gating payload accesses
    /// behind the enclosing tag check by emitting `CondBranch`
    /// terminators on failure paths), appends the guard's lowered
    /// operand stream + a `BoolAnd` fusion when present, and writes
    /// the final block's terminator as
    /// `CondBranch(<final i1>, body_block, failure_target)`.
    ///
    /// `failure_target` is the next arm's entry (`arms[idx+1]
    /// .check_blocks[0].id`) for non-final arms or `fallthrough_block`
    /// for the last arm. The same `failure_target` is threaded through
    /// every nested gating point inside the pattern, so any payload
    /// projection that would have dereferenced uninitialized memory
    /// when its enclosing tag check failed is unreachable instead.
    fn build_match_arm(
        &mut self,
        arm: &MatchArm,
        subject_ty: &Type,
        subject_ptr: &IROperand,
        ids: &MatchBlockIds,
        idx: usize,
        result_ty: &ResolvedMatchType,
    ) -> Result<LoweredMatchArm, String> {
        let entry_id = ids.arm_check_blocks[idx];
        let body_id = ids.arm_body_blocks[idx];
        let failure_target = if idx + 1 < ids.arm_check_blocks.len() {
            ids.arm_check_blocks[idx + 1]
        } else {
            ids.fallthrough_block
        };

        let check_blocks = self.build_match_arm_checks(
            arm,
            subject_ty,
            subject_ptr,
            entry_id,
            body_id,
            failure_target,
            idx,
        )?;

        let (mut body, trailing_op, trailing_ty) =
            self.build_match_arm_body(arm, body_id, ids.merge_block, idx)?;
        let trailing_value = trailing_op.map(|op| {
            self.maybe_pre_stage_arm_union_wrap(&mut body, op, trailing_ty.as_ref(), result_ty)
        });

        Ok(LoweredMatchArm {
            body,
            check_blocks,
            trailing_value,
        })
    }

    /// Build the per-arm check sub-CFG (legacy `Vec<IRBasicBlock>`
    /// shape consumed by [`Self::lower_pattern_into_arm`]). Returns
    /// the blocks in source order; [`Self::lower_match_expr`] drains
    /// them into the outer [`crate::CFGBuilder`].
    #[allow(clippy::too_many_arguments)]
    fn build_match_arm_checks(
        &mut self,
        arm: &MatchArm,
        subject_ty: &Type,
        subject_ptr: &IROperand,
        entry_id: IRBlockId,
        body_block: IRBlockId,
        failure_target: IRBlockId,
        idx: usize,
    ) -> Result<Vec<IRBasicBlock>, String> {
        let mut blocks = vec![placeholder_block(
            entry_id,
            format!("match_test_{idx}_entry"),
        )];

        let resolved = lower_pattern(&self.ctx(), &arm.pattern, subject_ty)?;
        let mut check_result =
            self.lower_pattern_into_arm(&resolved, subject_ptr, failure_target, &mut blocks)?;

        if let Some(guard) = &arm.guard {
            // Guards lower through a temporary single-block CFGBuilder
            // and drain instructions back into the arm's open block.
            // Nested control flow inside guards is rejected (matches
            // legacy behavior; `match Some(v) when v > 0 -> ...`-style
            // guards are pure expressions).
            let guard_open = blocks.last().unwrap().id;
            let mut tmp = CFGBuilder::new();
            tmp.add_block(guard_open, "match_guard".to_string());
            let (next, guard_operand, _) =
                self.lower_expr_to_operand(&mut tmp, guard_open, guard)?;
            let tmp_blocks = tmp.into_blocks();
            if tmp_blocks.len() != 1 || next.is_none() {
                return Err(format!(
                    "match arm {idx}: nested control flow inside guard not yet supported"
                ));
            }
            let guard_block = tmp_blocks.into_iter().next().unwrap();
            blocks
                .last_mut()
                .unwrap()
                .instructions
                .extend(guard_block.instructions);
            check_result = self.fuse_check_results(
                check_result,
                guard_operand,
                &mut blocks.last_mut().unwrap().instructions,
                ResolvedBinaryOp::BoolAnd,
            );
        }

        blocks.last_mut().unwrap().terminator = match check_result {
            IROperand::ConstBool(true) => IRTerminator::Branch(body_block),
            IROperand::ConstBool(false) => IRTerminator::Branch(failure_target),
            cond => IRTerminator::CondBranch {
                cond,
                then: body_block,
                otherwise: failure_target,
            },
        };
        Ok(blocks)
    }

    /// Lower the arm body into an [`IRBasicBlock`] using a temporary
    /// [`CFGBuilder`] (since [`Self::lower_statements_for_value`]
    /// builds blocks via the recursive CFG API). Drains the temp
    /// builder back into a single body [`IRBasicBlock`] consumed by
    /// [`Self::build_match_arm`]; multi-block bodies (control flow
    /// inside the arm) are flattened into a single body block today
    /// because match-arm body CFG support is post-Slice-3 work.
    fn build_match_arm_body(
        &mut self,
        arm: &MatchArm,
        body_id: IRBlockId,
        merge_id: IRBlockId,
        idx: usize,
    ) -> Result<(IRBasicBlock, Option<IROperand>, Option<Type>), String> {
        let mut tmp = CFGBuilder::new();
        tmp.add_block(body_id, format!("match_body_{idx}"));
        let (exit, trailing) = self.lower_statements_for_value(&mut tmp, body_id, &arm.body)?;
        let (trailing_op, trailing_ty) = match trailing {
            Some((op, ty)) => (Some(op), Some(ty)),
            None => (None, None),
        };
        // Match arms today support straight-line bodies (no nested
        // control flow that would mint extra blocks). Nested control
        // inside arms is post-Slice-3 work.
        let blocks = tmp.into_blocks();
        if blocks.len() != 1 {
            return Err(format!(
                "match arm {idx}: nested control flow inside body not yet supported \
                 (got {} blocks)",
                blocks.len()
            ));
        }
        let mut body = blocks.into_iter().next().unwrap();
        if exit.is_some() {
            body.terminator = IRTerminator::Branch(merge_id);
        }
        Ok((body, trailing_op, trailing_ty))
    }

    /// Pre-stage an [`IRInstruction::UnionWrap`] inside the body block
    /// when the lowered result strategy widens the arm's value and
    /// the trailing type isn't already a union. Returns the operand
    /// the merge phi should reference (either the wrapped dest or the
    /// original trailing operand untouched).
    fn maybe_pre_stage_arm_union_wrap(
        &mut self,
        body: &mut IRBasicBlock,
        trailing_op: IROperand,
        trailing_ty: Option<&Type>,
        result_ty: &ResolvedMatchType,
    ) -> IROperand {
        let target = match result_ty {
            ResolvedMatchType::UnionWrap { target } => target.clone(),
            ResolvedMatchType::Direct { .. } => return trailing_op,
        };
        if matches!(trailing_ty, Some(Type::Union(_))) {
            return trailing_op;
        }
        let source_ty = trailing_ty.cloned().unwrap_or(Type::Unknown);
        let dest = self.next_value_id();
        body.instructions.push(IRInstruction::UnionWrap {
            dest,
            value: trailing_op,
            source_ty,
            target_union: target,
        });
        IROperand::Local(dest)
    }

    /// Gated CFG builder for pattern lowering. Mirrors
    /// [`Self::lower_resolved_pattern`] but threads a `failure_target`
    /// IRBlockId through every constructor pattern so payload-bearing
    /// primitives (`PatternProjectVariantField`, `PatternUnionPayloadPtr`)
    /// land in successor blocks that are only entered when the
    /// enclosing tag check passed.
    ///
    /// Contract:
    ///
    /// - The "open block" is `blocks.last_mut().unwrap()`. Pattern
    ///   instructions are pushed into its `instructions`. When a
    ///   constructor pattern needs to gate, the open block is
    ///   terminated with a `CondBranch` and a fresh open block is
    ///   pushed onto `blocks`.
    /// - The returned [`IROperand`] is the i1 the *current open block
    ///   at exit* should test for the pattern's overall success. For
    ///   patterns that encoded their success/failure entirely via
    ///   control flow (constructors, `Or`), the return is
    ///   `IROperand::ConstBool(true)` -- reaching the end of the
    ///   sub-CFG is itself the success signal. For flat patterns
    ///   (`LiteralEq`, `EnumUnit`, `PatternBinaryMatch`), the return
    ///   is the i1 they emitted (typically `IROperand::Local`).
    ///
    /// `failure_target` is forwarded unchanged into nested constructor
    /// gating, so a deeply-nested pattern like
    /// `Some(TokenKind.Ident("and"))` produces three CondBranches
    /// (outer Some? inner Ident? literal "and"?), each branching
    /// directly to the same `failure_target` on miss. There is no
    /// per-arm "fail collector" block.
    fn lower_pattern_into_arm(
        &mut self,
        resolved: &ResolvedPattern,
        subject_ptr: &IROperand,
        failure_target: IRBlockId,
        blocks: &mut Vec<IRBasicBlock>,
    ) -> Result<IROperand, String> {
        match resolved {
            ResolvedPattern::AlwaysMatch => Ok(IROperand::ConstBool(true)),
            ResolvedPattern::Bind {
                name,
                ty,
                strict_llvm,
            } => {
                blocks
                    .last_mut()
                    .unwrap()
                    .instructions
                    .push(IRInstruction::PatternBindFromPtr {
                        name: name.clone(),
                        ty: ty.clone(),
                        source_ptr: subject_ptr.clone(),
                        strict_llvm: *strict_llvm,
                    });
                Ok(IROperand::ConstBool(true))
            }
            ResolvedPattern::Binary { segments } => {
                let dest = self.next_value_id();
                blocks
                    .last_mut()
                    .unwrap()
                    .instructions
                    .push(IRInstruction::PatternBinaryMatch {
                        dest,
                        subject_ptr: subject_ptr.clone(),
                        segments: segments.clone(),
                    });
                Ok(IROperand::Local(dest))
            }
            ResolvedPattern::EnumStruct {
                enum_key,
                variant,
                tag,
                fields,
            } => {
                let header = ConstructorHeader {
                    enum_key,
                    variant,
                    tag: *tag,
                };
                self.lower_enum_struct_into_arm(header, fields, subject_ptr, failure_target, blocks)
            }
            ResolvedPattern::EnumTuple {
                enum_key,
                variant,
                tag,
                elements,
            } => {
                let header = ConstructorHeader {
                    enum_key,
                    variant,
                    tag: *tag,
                };
                self.lower_enum_tuple_into_arm(
                    header,
                    elements,
                    subject_ptr,
                    failure_target,
                    blocks,
                )
            }
            ResolvedPattern::EnumUnit { enum_key, tag, .. } => {
                let dest = self.next_value_id();
                blocks
                    .last_mut()
                    .unwrap()
                    .instructions
                    .push(IRInstruction::PatternTagEq {
                        dest,
                        subject_ptr: subject_ptr.clone(),
                        enum_key: enum_key.clone(),
                        tag: *tag,
                    });
                Ok(IROperand::Local(dest))
            }
            ResolvedPattern::LiteralEq { lit, subject_ty } => {
                let dest = self.next_value_id();
                blocks
                    .last_mut()
                    .unwrap()
                    .instructions
                    .push(IRInstruction::PatternLiteralEq {
                        dest,
                        subject_ptr: subject_ptr.clone(),
                        subject_ty: subject_ty.clone(),
                        lit: lit.clone(),
                    });
                Ok(IROperand::Local(dest))
            }
            ResolvedPattern::Or(subs) => {
                self.lower_or_into_arm(subs, subject_ptr, failure_target, blocks)
            }
            ResolvedPattern::Struct { struct_key, fields } => self.lower_plain_struct_into_arm(
                struct_key,
                fields,
                subject_ptr,
                failure_target,
                blocks,
            ),
            ResolvedPattern::UnionMember {
                union_mangled,
                tag,
                member_ty,
                bind_name,
                ..
            } => {
                let tag_dest = self.next_value_id();
                blocks
                    .last_mut()
                    .unwrap()
                    .instructions
                    .push(IRInstruction::PatternTagEq {
                        dest: tag_dest,
                        subject_ptr: subject_ptr.clone(),
                        enum_key: union_mangled.as_str().to_string(),
                        tag: *tag,
                    });
                let payload_block = self.next_block_id();
                blocks.last_mut().unwrap().terminator = IRTerminator::CondBranch {
                    cond: IROperand::Local(tag_dest),
                    then: payload_block,
                    otherwise: failure_target,
                };
                blocks.push(placeholder_block(
                    payload_block,
                    "match_union_payload".to_string(),
                ));
                let payload_dest = self.next_value_id();
                blocks.last_mut().unwrap().instructions.extend([
                    IRInstruction::PatternUnionPayloadPtr {
                        dest: payload_dest,
                        subject_ptr: subject_ptr.clone(),
                        union_mangled: union_mangled.as_str().to_string(),
                    },
                    IRInstruction::PatternBindFromPtr {
                        name: bind_name.clone(),
                        ty: member_ty.clone(),
                        source_ptr: IROperand::Local(payload_dest),
                        strict_llvm: false,
                    },
                ]);
                Ok(IROperand::ConstBool(true))
            }
        }
    }

    /// Recursive heart of the (legacy) flat pattern lift. Used by
    /// [`Self::lower_pattern_to_instructions`] for the single-pattern
    /// entry point ([`compile_pattern`] in codegen, which serves
    /// `receive` arms and `expr matches Pattern`). Walks a
    /// [`ResolvedPattern`] in source order, threading `subject_ptr`
    /// (a pointer-typed [`IROperand`]) into every primitive's
    /// `subject_ptr` slot. Emits pattern-test instructions into `out`
    /// and returns the [`IROperand`] representing the final i1.
    ///
    /// Note: this flat shape unconditionally emits payload-projection
    /// instructions even when the enclosing tag check is false. The
    /// match-arm path uses the gated CFG builder
    /// [`Self::lower_pattern_into_arm`] instead, which is what fixes
    /// the GAPS literal-payload-deref segfault for `match`. The same
    /// fix has not been applied to `compile_pattern` yet -- a
    /// follow-up (lifting `receive` / `matches` to the same gated
    /// shape) is tracked separately.
    fn lower_resolved_pattern(
        &mut self,
        resolved: &ResolvedPattern,
        subject_ptr: &IROperand,
        out: &mut Vec<IRInstruction>,
    ) -> Result<IROperand, String> {
        match resolved {
            ResolvedPattern::AlwaysMatch => Ok(IROperand::ConstBool(true)),
            ResolvedPattern::Bind {
                name,
                ty,
                strict_llvm,
            } => {
                out.push(IRInstruction::PatternBindFromPtr {
                    name: name.clone(),
                    ty: ty.clone(),
                    source_ptr: subject_ptr.clone(),
                    strict_llvm: *strict_llvm,
                });
                Ok(IROperand::ConstBool(true))
            }
            ResolvedPattern::Binary { segments } => {
                let dest = self.next_value_id();
                out.push(IRInstruction::PatternBinaryMatch {
                    dest,
                    subject_ptr: subject_ptr.clone(),
                    segments: segments.clone(),
                });
                Ok(IROperand::Local(dest))
            }
            ResolvedPattern::EnumStruct {
                enum_key,
                variant,
                tag,
                fields,
            } => self.lower_enum_struct_pattern(enum_key, variant, *tag, fields, subject_ptr, out),
            ResolvedPattern::EnumTuple {
                enum_key,
                variant,
                tag,
                elements,
            } => self.lower_enum_tuple_pattern(enum_key, variant, *tag, elements, subject_ptr, out),
            ResolvedPattern::EnumUnit { enum_key, tag, .. } => {
                let dest = self.next_value_id();
                out.push(IRInstruction::PatternTagEq {
                    dest,
                    subject_ptr: subject_ptr.clone(),
                    enum_key: enum_key.clone(),
                    tag: *tag,
                });
                Ok(IROperand::Local(dest))
            }
            ResolvedPattern::LiteralEq { lit, subject_ty } => {
                let dest = self.next_value_id();
                out.push(IRInstruction::PatternLiteralEq {
                    dest,
                    subject_ptr: subject_ptr.clone(),
                    subject_ty: subject_ty.clone(),
                    lit: lit.clone(),
                });
                Ok(IROperand::Local(dest))
            }
            ResolvedPattern::Or(subs) => {
                let mut result = IROperand::ConstBool(false);
                for sub in subs {
                    let sub_result = self.lower_resolved_pattern(sub, subject_ptr, out)?;
                    result =
                        self.fuse_check_results(result, sub_result, out, ResolvedBinaryOp::BoolOr);
                }
                Ok(result)
            }
            ResolvedPattern::Struct { struct_key, fields } => {
                self.lower_plain_struct_pattern(struct_key, fields, subject_ptr, out)
            }
            ResolvedPattern::UnionMember {
                union_mangled,
                tag,
                member_ty,
                bind_name,
                ..
            } => {
                let tag_dest = self.next_value_id();
                out.push(IRInstruction::PatternTagEq {
                    dest: tag_dest,
                    subject_ptr: subject_ptr.clone(),
                    enum_key: union_mangled.as_str().to_string(),
                    tag: *tag,
                });
                let payload_dest = self.next_value_id();
                out.push(IRInstruction::PatternUnionPayloadPtr {
                    dest: payload_dest,
                    subject_ptr: subject_ptr.clone(),
                    union_mangled: union_mangled.as_str().to_string(),
                });
                out.push(IRInstruction::PatternBindFromPtr {
                    name: bind_name.clone(),
                    ty: member_ty.clone(),
                    source_ptr: IROperand::Local(payload_dest),
                    strict_llvm: false,
                });
                Ok(IROperand::Local(tag_dest))
            }
        }
    }

    /// Lower a [`ResolvedPattern::EnumTuple`]: emit a tag check, then
    /// per element emit a [`IRInstruction::PatternProjectVariantField`]
    /// that produces the field's alloca pointer, recurse into the
    /// sub-pattern, and `BoolAnd`-fuse the result. Mirrors the legacy
    /// `emit_pattern`'s tuple-variant branch.
    fn lower_enum_tuple_pattern(
        &mut self,
        enum_key: &str,
        variant: &str,
        tag: u8,
        elements: &[(Type, ResolvedPattern)],
        subject_ptr: &IROperand,
        out: &mut Vec<IRInstruction>,
    ) -> Result<IROperand, String> {
        let tag_dest = self.next_value_id();
        out.push(IRInstruction::PatternTagEq {
            dest: tag_dest,
            subject_ptr: subject_ptr.clone(),
            enum_key: enum_key.to_string(),
            tag,
        });
        let mut result = IROperand::Local(tag_dest);
        for (i, (field_ty, sub)) in elements.iter().enumerate() {
            let field_ptr_dest = self.next_value_id();
            out.push(IRInstruction::PatternProjectVariantField {
                dest: field_ptr_dest,
                subject_ptr: subject_ptr.clone(),
                enum_key: enum_key.to_string(),
                variant: variant.to_string(),
                field_index: i as u32,
                field_ty: field_ty.clone(),
                name_hint: format!("tp{i}"),
            });
            let sub_result =
                self.lower_resolved_pattern(sub, &IROperand::Local(field_ptr_dest), out)?;
            result = self.fuse_check_results(result, sub_result, out, ResolvedBinaryOp::BoolAnd);
        }
        Ok(result)
    }

    /// Lower a [`ResolvedPattern::EnumStruct`]: emit a tag check, then
    /// per field emit a [`IRInstruction::PatternProjectVariantField`]
    /// followed by a recursive sub-pattern (with `BoolAnd` fusion).
    /// Mirrors the legacy `emit_pattern`'s struct-variant branch.
    fn lower_enum_struct_pattern(
        &mut self,
        enum_key: &str,
        variant: &str,
        tag: u8,
        fields: &[ResolvedFieldPattern],
        subject_ptr: &IROperand,
        out: &mut Vec<IRInstruction>,
    ) -> Result<IROperand, String> {
        let tag_dest = self.next_value_id();
        out.push(IRInstruction::PatternTagEq {
            dest: tag_dest,
            subject_ptr: subject_ptr.clone(),
            enum_key: enum_key.to_string(),
            tag,
        });
        let mut result = IROperand::Local(tag_dest);
        for fp in fields {
            let field_ptr_dest = self.next_value_id();
            out.push(IRInstruction::PatternProjectVariantField {
                dest: field_ptr_dest,
                subject_ptr: subject_ptr.clone(),
                enum_key: enum_key.to_string(),
                variant: variant.to_string(),
                field_index: fp.field_index,
                field_ty: fp.field_type.clone(),
                name_hint: fp.name.clone(),
            });
            let sub_result =
                self.lower_resolved_pattern(&fp.sub, &IROperand::Local(field_ptr_dest), out)?;
            result = self.fuse_check_results(result, sub_result, out, ResolvedBinaryOp::BoolAnd);
        }
        Ok(result)
    }

    /// Lower a [`ResolvedPattern::EnumTuple`] into the arm's check
    /// sub-CFG with payload-gating. Emits `PatternTagEq` into the open
    /// block, terminates it with `CondBranch(tag, payload_block,
    /// failure_target)`, opens the payload block, and projects each
    /// element field there. Sub-patterns recurse via
    /// [`Self::lower_pattern_into_arm`] (gating further if they
    /// themselves are constructors). Between elements, a non-trivial
    /// sub-result triggers another gate so the next field's projection
    /// doesn't execute when the prior element's check failed.
    fn lower_enum_tuple_into_arm(
        &mut self,
        header: ConstructorHeader<'_>,
        elements: &[(Type, ResolvedPattern)],
        subject_ptr: &IROperand,
        failure_target: IRBlockId,
        blocks: &mut Vec<IRBasicBlock>,
    ) -> Result<IROperand, String> {
        self.gate_tag_check(
            header.enum_key,
            header.tag,
            subject_ptr,
            failure_target,
            blocks,
        );
        for (i, (field_ty, sub)) in elements.iter().enumerate() {
            let field_ptr_dest = self.next_value_id();
            blocks.last_mut().unwrap().instructions.push(
                IRInstruction::PatternProjectVariantField {
                    dest: field_ptr_dest,
                    subject_ptr: subject_ptr.clone(),
                    enum_key: header.enum_key.to_string(),
                    variant: header.variant.to_string(),
                    field_index: i as u32,
                    field_ty: field_ty.clone(),
                    name_hint: format!("tp{i}"),
                },
            );
            let sub_result = self.lower_pattern_into_arm(
                sub,
                &IROperand::Local(field_ptr_dest),
                failure_target,
                blocks,
            )?;
            let is_last = i + 1 == elements.len();
            if is_last {
                return Ok(sub_result);
            }
            if let Some(short_circuit) =
                self.gate_intermediate_field(sub_result, failure_target, blocks)
            {
                return Ok(short_circuit);
            }
        }
        Ok(IROperand::ConstBool(true))
    }

    /// Lower a [`ResolvedPattern::EnumStruct`] into the arm's check
    /// sub-CFG with payload-gating. Same shape as
    /// [`Self::lower_enum_tuple_into_arm`] but with named fields: each
    /// field's sub-pattern recurses through
    /// [`Self::lower_pattern_into_arm`]. Bindings happen via the
    /// recursion's `ResolvedPattern::Bind` arm, which emits the
    /// `PatternBindFromPtr` only inside the payload block (never
    /// speculatively).
    fn lower_enum_struct_into_arm(
        &mut self,
        header: ConstructorHeader<'_>,
        fields: &[ResolvedFieldPattern],
        subject_ptr: &IROperand,
        failure_target: IRBlockId,
        blocks: &mut Vec<IRBasicBlock>,
    ) -> Result<IROperand, String> {
        self.gate_tag_check(
            header.enum_key,
            header.tag,
            subject_ptr,
            failure_target,
            blocks,
        );
        for (i, fp) in fields.iter().enumerate() {
            let field_ptr_dest = self.next_value_id();
            blocks.last_mut().unwrap().instructions.push(
                IRInstruction::PatternProjectVariantField {
                    dest: field_ptr_dest,
                    subject_ptr: subject_ptr.clone(),
                    enum_key: header.enum_key.to_string(),
                    variant: header.variant.to_string(),
                    field_index: fp.field_index,
                    field_ty: fp.field_type.clone(),
                    name_hint: fp.name.clone(),
                },
            );
            let sub_result = self.lower_pattern_into_arm(
                &fp.sub,
                &IROperand::Local(field_ptr_dest),
                failure_target,
                blocks,
            )?;
            let is_last = i + 1 == fields.len();
            if is_last {
                return Ok(sub_result);
            }
            if let Some(short_circuit) =
                self.gate_intermediate_field(sub_result, failure_target, blocks)
            {
                return Ok(short_circuit);
            }
        }
        Ok(IROperand::ConstBool(true))
    }

    /// Lower a [`ResolvedPattern::Struct`] into the arm's check sub-CFG.
    /// Plain (non-enum) structs have no tag and no untagged-union
    /// payload memory, so projection is unconditionally safe -- there's
    /// no payload-block split. Per-field
    /// [`IRInstruction::PatternProjectStructField`] emits into the open
    /// block, then recurses into the field's sub-pattern with
    /// [`Self::gate_intermediate_field`] sequencing literal-bearing
    /// siblings.
    fn lower_plain_struct_into_arm(
        &mut self,
        struct_key: &str,
        fields: &[ResolvedFieldPattern],
        subject_ptr: &IROperand,
        failure_target: IRBlockId,
        blocks: &mut Vec<IRBasicBlock>,
    ) -> Result<IROperand, String> {
        if fields.is_empty() {
            return Ok(IROperand::ConstBool(true));
        }
        for (i, fp) in fields.iter().enumerate() {
            let field_ptr_dest = self.next_value_id();
            blocks.last_mut().unwrap().instructions.push(
                IRInstruction::PatternProjectStructField {
                    dest: field_ptr_dest,
                    subject_ptr: subject_ptr.clone(),
                    struct_key: struct_key.to_string(),
                    field_index: fp.field_index,
                    field_ty: fp.field_type.clone(),
                    name_hint: fp.name.clone(),
                },
            );
            let sub_result = self.lower_pattern_into_arm(
                &fp.sub,
                &IROperand::Local(field_ptr_dest),
                failure_target,
                blocks,
            )?;
            let is_last = i + 1 == fields.len();
            if is_last {
                return Ok(sub_result);
            }
            if let Some(short_circuit) =
                self.gate_intermediate_field(sub_result, failure_target, blocks)
            {
                return Ok(short_circuit);
            }
        }
        Ok(IROperand::ConstBool(true))
    }

    /// Lower a [`ResolvedPattern::Struct`] in the legacy flat instruction
    /// stream (single-pattern path: `compile_pattern` for `receive` arms
    /// and `expr matches Pattern`). Mirror of
    /// [`Self::lower_enum_struct_pattern`] minus the tag check; safe in
    /// the flat shape because struct projection has no payload-deref
    /// vulnerability.
    fn lower_plain_struct_pattern(
        &mut self,
        struct_key: &str,
        fields: &[ResolvedFieldPattern],
        subject_ptr: &IROperand,
        out: &mut Vec<IRInstruction>,
    ) -> Result<IROperand, String> {
        let mut result = IROperand::ConstBool(true);
        for fp in fields {
            let field_ptr_dest = self.next_value_id();
            out.push(IRInstruction::PatternProjectStructField {
                dest: field_ptr_dest,
                subject_ptr: subject_ptr.clone(),
                struct_key: struct_key.to_string(),
                field_index: fp.field_index,
                field_ty: fp.field_type.clone(),
                name_hint: fp.name.clone(),
            });
            let sub_result =
                self.lower_resolved_pattern(&fp.sub, &IROperand::Local(field_ptr_dest), out)?;
            result = self.fuse_check_results(result, sub_result, out, ResolvedBinaryOp::BoolAnd);
        }
        Ok(result)
    }

    /// Lower a [`ResolvedPattern::Or`] into the arm's check sub-CFG.
    /// Each sub-pattern lowers with `failure_target = next_sub_entry`
    /// (or the outer `failure_target` for the last sub) so that a
    /// failed sub-pattern naturally tries the next alternative. Each
    /// sub's "success" terminator branches to a shared `or_success`
    /// block; subsequent recursion continues from that block. With all
    /// subs flat-true (the unusual case), the resulting sub-CFG is
    /// just `Branch(or_success)`.
    fn lower_or_into_arm(
        &mut self,
        subs: &[ResolvedPattern],
        subject_ptr: &IROperand,
        failure_target: IRBlockId,
        blocks: &mut Vec<IRBasicBlock>,
    ) -> Result<IROperand, String> {
        if subs.is_empty() {
            return Ok(IROperand::ConstBool(false));
        }
        let or_success = self.next_block_id();
        let sub_entry_ids: Vec<IRBlockId> = std::iter::once(blocks.last().unwrap().id)
            .chain((1..subs.len()).map(|_| self.next_block_id()))
            .collect();
        for (i, sub) in subs.iter().enumerate() {
            if i > 0 {
                blocks.push(placeholder_block(
                    sub_entry_ids[i],
                    format!("match_or_alt_{i}"),
                ));
            }
            let next_failure = if i + 1 < subs.len() {
                sub_entry_ids[i + 1]
            } else {
                failure_target
            };
            let sub_result = self.lower_pattern_into_arm(sub, subject_ptr, next_failure, blocks)?;
            blocks.last_mut().unwrap().terminator = match sub_result {
                IROperand::ConstBool(true) => IRTerminator::Branch(or_success),
                IROperand::ConstBool(false) => IRTerminator::Branch(next_failure),
                cond => IRTerminator::CondBranch {
                    cond,
                    then: or_success,
                    otherwise: next_failure,
                },
            };
        }
        blocks.push(placeholder_block(
            or_success,
            "match_or_success".to_string(),
        ));
        Ok(IROperand::ConstBool(true))
    }

    /// Emit the outer tag-check + gate for a constructor pattern.
    /// Pushes a `PatternTagEq` into the open block, terminates it with
    /// `CondBranch(tag, payload_block, failure_target)`, and opens a
    /// fresh payload block on `blocks`. Subsequent field projections
    /// land in the payload block.
    fn gate_tag_check(
        &mut self,
        enum_key: &str,
        tag: u8,
        subject_ptr: &IROperand,
        failure_target: IRBlockId,
        blocks: &mut Vec<IRBasicBlock>,
    ) {
        let tag_dest = self.next_value_id();
        blocks
            .last_mut()
            .unwrap()
            .instructions
            .push(IRInstruction::PatternTagEq {
                dest: tag_dest,
                subject_ptr: subject_ptr.clone(),
                enum_key: enum_key.to_string(),
                tag,
            });
        let payload_block = self.next_block_id();
        blocks.last_mut().unwrap().terminator = IRTerminator::CondBranch {
            cond: IROperand::Local(tag_dest),
            then: payload_block,
            otherwise: failure_target,
        };
        blocks.push(placeholder_block(
            payload_block,
            "match_payload".to_string(),
        ));
    }

    /// Inter-field gating helper for tuple/struct constructor patterns.
    /// Returns `Some(short_circuit_result)` when the sub-pattern's
    /// result is a compile-time false (the entire constructor fails;
    /// caller should bail), `None` when the open block continues
    /// without a gate (sub-result is trivially true), and `None` plus
    /// a fresh open block when a runtime check is required (the open
    /// block is terminated with `CondBranch(sub_result, next_field,
    /// failure_target)`).
    fn gate_intermediate_field(
        &mut self,
        sub_result: IROperand,
        failure_target: IRBlockId,
        blocks: &mut Vec<IRBasicBlock>,
    ) -> Option<IROperand> {
        match sub_result {
            IROperand::ConstBool(true) => None,
            IROperand::ConstBool(false) => {
                blocks.last_mut().unwrap().terminator = IRTerminator::Branch(failure_target);
                Some(IROperand::ConstBool(false))
            }
            cond => {
                let next_field_block = self.next_block_id();
                blocks.last_mut().unwrap().terminator = IRTerminator::CondBranch {
                    cond,
                    then: next_field_block,
                    otherwise: failure_target,
                };
                blocks.push(placeholder_block(
                    next_field_block,
                    "match_field_continue".to_string(),
                ));
                None
            }
        }
    }

    /// Fuse two i1 [`IROperand`]s with the given boolean op, emitting a
    /// [`IRInstruction::BinaryOp`] only when neither operand is a
    /// constant short-circuit. The constant-folding (e.g. `BoolAnd(true,
    /// x) -> x`) keeps the emitted instruction stream tight for arms
    /// like `Some(v) -> ...` whose `Bind` sub-pattern returns
    /// `ConstBool(true)`.
    fn fuse_check_results(
        &mut self,
        lhs: IROperand,
        rhs: IROperand,
        check: &mut Vec<IRInstruction>,
        op: ResolvedBinaryOp,
    ) -> IROperand {
        match (&op, &lhs, &rhs) {
            (ResolvedBinaryOp::BoolAnd, IROperand::ConstBool(true), _) => return rhs,
            (ResolvedBinaryOp::BoolAnd, _, IROperand::ConstBool(true)) => return lhs,
            (ResolvedBinaryOp::BoolAnd, IROperand::ConstBool(false), _)
            | (ResolvedBinaryOp::BoolAnd, _, IROperand::ConstBool(false)) => {
                return IROperand::ConstBool(false);
            }
            (ResolvedBinaryOp::BoolOr, IROperand::ConstBool(false), _) => return rhs,
            (ResolvedBinaryOp::BoolOr, _, IROperand::ConstBool(false)) => return lhs,
            (ResolvedBinaryOp::BoolOr, IROperand::ConstBool(true), _)
            | (ResolvedBinaryOp::BoolOr, _, IROperand::ConstBool(true)) => {
                return IROperand::ConstBool(true);
            }
            _ => {}
        }
        let dest = self.next_value_id();
        check.push(IRInstruction::BinaryOp { dest, op, lhs, rhs });
        IROperand::Local(dest)
    }
}

/// Borrowed view of a constructor pattern's enum/variant/tag triple,
/// shared between `lower_enum_tuple_into_arm` and
/// `lower_enum_struct_into_arm` so each helper stays under the
/// argument-count lint.
struct ConstructorHeader<'a> {
    enum_key: &'a str,
    variant: &'a str,
    tag: u8,
}

/// Construct a fresh [`IRBasicBlock`] with the given id and label, an
/// empty instruction list, and a placeholder `Branch` terminator that
/// the gating helpers in [`Lowerer`] overwrite as soon as they know
/// the real successor. Using `Branch(id)` (a self-branch) as the
/// placeholder keeps the IR well-formed if a builder bug ever leaves
/// it unwritten, surfacing the issue as an LLVM-side infinite loop
/// rather than an enum-variant mismatch.
fn placeholder_block(id: IRBlockId, label: String) -> IRBasicBlock {
    IRBasicBlock {
        id,
        instructions: Vec::new(),
        label,
        terminator: IRTerminator::Branch(id),
    }
}
