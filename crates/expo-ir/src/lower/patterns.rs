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
use crate::blocks::{IRBlockId, IRTerminator};
use crate::identity::MonomorphizedTypeIdentifier;
use crate::lower::ctx::LowerCtx;
use crate::lower::mangling::try_parse_mangled_name;
use crate::lower::types::{
    find_type_current, monomorphize_type, resolve_name_current, resolve_type_expr,
};
use crate::resolved::match_expr::{IRMatch, IRMatchArm, ResolvedMatchType};
use crate::resolved::ops::ResolvedBinaryOp;
use crate::resolved::patterns::{ResolvedFieldPattern, ResolvedLiteral, ResolvedPattern};
use crate::util::parse_int_literal;
use crate::values::{IRInstruction, IROperand, IRValueId};

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
    let mut out = Vec::with_capacity(fields.len());
    for fp in fields {
        let (idx, (_, field_type)) = expected
            .iter()
            .enumerate()
            .find(|(_, (n, _))| *n == fp.name)
            .ok_or_else(|| format!("unknown field `{}` in {enum_key}.{variant}", fp.name))?;
        let inner_ty = unwrap_indirect(field_type);
        let sub = match &fp.pattern {
            Some(p) => Some(lower_pattern(ctx, p, inner_ty)?),
            None => None,
        };
        out.push(ResolvedFieldPattern {
            name: fp.name.clone(),
            field_index: idx as u32,
            field_type: field_type.clone(),
            sub,
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

/// Per-arm + global block / value identifiers minted for an [`IRMatch`].
/// Pulled out into a struct so [`Lowerer::lower_match_expr`] can hand
/// the ids to the per-arm builder without threading them as a long
/// parameter list.
struct MatchBlockIds {
    arm_body_blocks: Vec<IRBlockId>,
    arm_check_blocks: Vec<IRBlockId>,
    fallthrough_block: IRBlockId,
    merge_block: IRBlockId,
    merge_phi_dest: IRValueId,
    subject_value: IRValueId,
}

/// Outcome of lowering a single AST [`Pattern`] into an instruction
/// stream. Slice 5b's central data structure: replaces the codegen-side
/// `emit_pattern` walker with a fully IR-encoded representation that
/// emission walks through the standard `execute_instructions` machinery.
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
/// Why one stream and not "tests in check, binds in body": Expo's
/// match guards (`Some(v) when v > 0`) reference pattern bindings
/// from the same arm. The guard is evaluated in the arm's check
/// block, before the cond branch fires, so the bindings must already
/// be live in `Compiler.fn_state.variables` at guard-evaluation
/// time. Binds therefore run unconditionally in the check block; the
/// codegen emitter wraps each arm (check + body) in a
/// `Compiler.fn_state.variables` clone/restore so the bindings
/// scope to the arm rather than leaking forward to subsequent arms.
/// The 5b lift moved the binding *setup* into IR (visible as
/// [`IRInstruction::PatternBindFromPtr`]); the per-arm *scoping*
/// stays in codegen because the variables map carries LLVM-typed
/// allocas that aren't part of the IR surface.
///
/// `check_result` is the [`IROperand`] consumers of the lowered
/// pattern reference for the cond branch (`match` arms) or the
/// returned `IntValue` (`compile_pattern` for `receive` arms).
pub struct LoweredPattern {
    pub check_result: IROperand,
    pub instructions: Vec<IRInstruction>,
}

impl<'a> Lowerer<'a> {
    /// Lowers a `match` expression into an [`IRMatch`].
    ///
    /// Pattern resolution flows through [`lower_pattern`] (per arm) and
    /// then [`Self::lower_pattern_to_instructions`] to produce each
    /// arm's `check_instructions` + `bind_instructions` streams.
    /// `result_ty` is the typecheck-derived Direct vs UnionWrap
    /// strategy, computed up-front by the codegen shim and threaded in.
    ///
    /// Guards lift to operand-emitting instructions appended to the
    /// arm's `check_instructions`; the result operand is `BoolAnd`-ed
    /// with the pattern's i1 to produce the final cond. No codegen-side
    /// guard handling remains.
    ///
    /// The arm chain: `arms[i].check_terminator.otherwise` points at
    /// `arms[i+1].check_block` for `i < N-1`, and at `fallthrough_block`
    /// for the last arm. `fallthrough_block` is the all-patterns-failed
    /// landing pad (matches legacy semantics); the inline-synthesized
    /// merge phi registers an `undef` incoming from it when
    /// value-producing.
    pub fn lower_match_expr(
        &mut self,
        subject_ty: Type,
        arms: &[MatchArm],
        result_ty: ResolvedMatchType,
    ) -> Result<IRMatch, String> {
        let ids = self.mint_match_block_ids(arms.len());
        let subject_operand = IROperand::Local(ids.subject_value);
        let mut lowered_arms = Vec::with_capacity(arms.len());
        for (i, arm) in arms.iter().enumerate() {
            lowered_arms.push(self.build_match_arm(arm, &subject_ty, &subject_operand, &ids, i)?);
        }
        Ok(IRMatch {
            arms: lowered_arms,
            fallthrough_block: ids.fallthrough_block,
            merge_block: ids.merge_block,
            merge_phi_dest: ids.merge_phi_dest,
            result_ty,
            subject_ty,
            subject_value: ids.subject_value,
        })
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

    /// Mints fresh per-arm and global identifiers for an [`IRMatch`]:
    /// one `check_block` and one `body_block` per arm, plus shared
    /// `fallthrough_block`, `merge_block`, `merge_phi_dest`, and
    /// `subject_value` (the SSA slot the codegen walker stuffs the
    /// match subject's pointer into before running per-arm checks).
    fn mint_match_block_ids(&mut self, arm_count: usize) -> MatchBlockIds {
        let arm_check_blocks: Vec<_> = (0..arm_count).map(|_| self.next_block_id()).collect();
        let arm_body_blocks: Vec<_> = (0..arm_count).map(|_| self.next_block_id()).collect();
        let fallthrough_block = self.next_block_id();
        let merge_block = self.next_block_id();
        let merge_phi_dest = self.next_value_id();
        let subject_value = self.next_value_id();
        MatchBlockIds {
            arm_body_blocks,
            arm_check_blocks,
            fallthrough_block,
            merge_block,
            merge_phi_dest,
            subject_value,
        }
    }

    /// Builds a single [`IRMatchArm`]. Lowers the arm's pattern via
    /// [`Self::lower_pattern_to_instructions`], appends the guard's
    /// lowered operand stream + a `BoolAnd` fusion when present, and
    /// wires the canonicalized cond-branch (`then = body_block`,
    /// `otherwise = next_arm.check_block` or `fallthrough_block`).
    fn build_match_arm(
        &mut self,
        arm: &MatchArm,
        subject_ty: &Type,
        subject_ptr: &IROperand,
        ids: &MatchBlockIds,
        idx: usize,
    ) -> Result<IRMatchArm, String> {
        let mut lowered =
            self.lower_pattern_to_instructions(&arm.pattern, subject_ty, subject_ptr.clone())?;
        let mut check_result = lowered.check_result;
        if let Some(guard) = &arm.guard {
            let guard_operand = self.lower_expr_to_operand(&mut lowered.instructions, guard);
            check_result = self.fuse_check_results(
                check_result,
                guard_operand,
                &mut lowered.instructions,
                ResolvedBinaryOp::BoolAnd,
            );
        }
        let next_block = if idx + 1 < ids.arm_check_blocks.len() {
            ids.arm_check_blocks[idx + 1]
        } else {
            ids.fallthrough_block
        };
        Ok(IRMatchArm {
            body_block: ids.arm_body_blocks[idx],
            body_stmts: arm.body.clone(),
            body_terminator: IRTerminator::Branch(ids.merge_block),
            check_block: ids.arm_check_blocks[idx],
            check_instructions: lowered.instructions,
            check_terminator: IRTerminator::CondBranch {
                cond: check_result,
                then: ids.arm_body_blocks[idx],
                otherwise: next_block,
            },
        })
    }

    /// Recursive heart of the pattern lift. Walks a [`ResolvedPattern`]
    /// in source order, threading `subject_ptr` (a pointer-typed
    /// [`IROperand`]) into every primitive's `subject_ptr` slot. Emits
    /// pattern-test instructions into `check`, binding setup
    /// instructions into `bind`, and returns the [`IROperand`]
    /// representing the final i1 (a [`IROperand::Local`] for
    /// non-trivial patterns; [`IROperand::ConstBool(true)`] for
    /// `AlwaysMatch` / `Bind` patterns, which always succeed).
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
    /// followed by either a recursive sub-pattern (with `BoolAnd`
    /// fusion) or a direct [`IRInstruction::PatternBindFromPtr`] when
    /// the field has no sub-pattern (the field name doubles as the
    /// binding name). Mirrors the legacy `emit_pattern`'s struct-variant
    /// branch.
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
            if let Some(sub) = &fp.sub {
                let sub_result =
                    self.lower_resolved_pattern(sub, &IROperand::Local(field_ptr_dest), out)?;
                result =
                    self.fuse_check_results(result, sub_result, out, ResolvedBinaryOp::BoolAnd);
            } else {
                out.push(IRInstruction::PatternBindFromPtr {
                    name: fp.name.clone(),
                    ty: unwrap_indirect(&fp.field_type).clone(),
                    source_ptr: IROperand::Local(field_ptr_dest),
                    strict_llvm: false,
                });
            }
        }
        Ok(result)
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
