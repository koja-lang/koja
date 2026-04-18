//! Pattern matching compilation: `match` expressions and pattern-to-boolean
//! lowering for all pattern variants (bindings, literals, enum variants, typed
//! bindings, constructors).
//!
//! The pattern pipeline is split into two phases:
//!
//! - [`lower_pattern`] consumes the AST `Pattern` plus the subject's `Type`
//!   and produces an [`expo_ir::resolved::patterns::ResolvedPattern`]. All
//!   package-aware enum-key resolution (`alpha.Status` vs bare `Status`),
//!   variant tag lookup, payload-shape lookup, and field-index resolution
//!   happens here. This is the only side that touches `compiler.types`,
//!   `compiler.type_ctx`, or any string-keyed resolution helper.
//!
//! - [`emit_pattern`] consumes the `ResolvedPattern` and emits LLVM IR. It
//!   only performs deterministic `Type` -> `BasicTypeEnum` translations and
//!   builder calls; it never resolves a name.
//!
//! [`compile_pattern`] is the public entry point and a thin
//! `lower(...)?.then(emit(...))` shim, kept for callers in `expr.rs`,
//! `compile_match`, and the binary-pattern code path.
//!
//! ### Why the GEPIndex panic is unreachable here
//!
//! The deferred panic at `build_struct_gep` for the payload pointer was
//! produced by code that asked for the payload of a unit variant. After this
//! split, [`ResolvedPattern::EnumUnit`](expo_ir::resolved::patterns::ResolvedPattern::EnumUnit)
//! carries no payload information, and the emission match arm for it does
//! not call [`get_payload_ptr`]. A unit variant cannot be lowered into any
//! shape that triggers a payload GEP -- the bug becomes structurally
//! impossible.

use crate::binary::patterns::compile_binary_pattern;
use crate::drop::Ownership;
use expo_ast::ast::{
    BinarySegment, Expr, ExprKind, FieldPattern, Literal, MatchArm, Pattern, Statement,
};
use expo_ir::resolved::match_expr::{ResolvedMatch, ResolvedMatchType};
use expo_ir::resolved::patterns::{ResolvedFieldPattern, ResolvedLiteral, ResolvedPattern};
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{
    Type, TypeIdentifier, mangle_name, mangle_type, named, unwrap_indirect,
};
use inkwell::FloatPredicate;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::compile_expr;
use crate::structs::load_maybe_indirect;
use crate::types::to_llvm_type;
use crate::util::parse_int_literal;

use super::compile_body_as_value;

/// Compiles a `match` expression. Patterns are tested sequentially; the first
/// matching arm executes. Bindings introduced by patterns are scoped to their
/// arm. Returns a phi value when all arms produce a value of the same type.
///
/// Today's ordering is: emit the subject first (so its post-emit Expo type is
/// available), then [`lower_match`] resolves all arm patterns and the
/// result-type strategy from typecheck info, then [`emit_match`] emits the
/// per-arm scaffolding and final phi.
///
/// ### Why pre-emit (and not pure lower-then-emit)
///
/// The lower/emit split would prefer lowering to run *before* any emission.
/// That requires `subject.resolved_type` (and every other expression's
/// `resolved_type`) to be populated by typecheck on the AST that codegen
/// monomorphizes from. It currently isn't: collect.rs clones impl blocks
/// into `ctx.generic_impl_asts` / `ctx.specialized_impl_asts` *before*
/// check.rs runs, and codegen reads those clones rather than the
/// typechecked AST in `module.items`. Until that AST-clone story is fixed
/// (see fix-generic-impl-typecheck plan, Stage 5), we lean on the
/// post-emit `subject_tv.expo_type`. The Ident/Self_ branch in
/// `resolve_subject_ty` is a final defensive fallback for residual gaps.
///
/// ### Behavior change vs the pre-IR implementation
///
/// The "Direct vs UnionWrap" strategy is a typecheck decision (taken in
/// [`lower_match`]). If LLVM phi types disagree with that decision at
/// emission time, [`emit_match`] returns an error rather than silently
/// returning `Ok(None)`. This surfaces typecheck/codegen disagreements
/// instead of swallowing them.
pub fn compile_match<'ctx>(
    compiler: &mut Compiler<'ctx>,
    subject: &Expr,
    arms: &[MatchArm],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let subject_tv =
        compile_expr(compiler, subject, function)?.ok_or("match subject produced no value")?;
    let subject_ty = resolve_subject_ty(compiler, subject, &subject_tv.expo_type);
    let resolved = lower_match(compiler, &subject_ty, arms)?;
    emit_match(compiler, &resolved, subject_tv.value, arms, function)
}

/// Picks the most specific Expo type available for the match subject. Prefers
/// the post-emit `expo_type` (codegen has full monomorphization context, even
/// inside generic impl bodies whose typechecked `resolved_type` doesn't reach
/// the cached AST clones); falls back to the typecheck-populated
/// `resolved_type` and finally to the variable-binding heuristic. Always
/// returns a usable type when any source has one; only returns `Type::Unknown`
/// when every source agrees there is none.
fn resolve_subject_ty(compiler: &Compiler<'_>, subject: &Expr, post_emit_ty: &Type) -> Type {
    if !matches!(post_emit_ty, Type::Unknown) {
        return post_emit_ty.clone();
    }
    if let Some(ty) = subject.resolved_type.as_ref() {
        let substituted = compiler.monomorphize_type(ty);
        if !matches!(substituted, Type::Unknown) {
            return substituted;
        }
    }
    if let ExprKind::Ident { name, .. } = &subject.kind
        && let Some((_, ty, _)) = compiler.fn_state.variables.get(name)
    {
        return ty.clone();
    }
    if matches!(subject.kind, ExprKind::Self_)
        && let Some((_, ty, _)) = compiler.fn_state.variables.get("self")
    {
        return ty.clone();
    }
    Type::Unknown
}

// ---------------------------------------------------------------------------
// Lowering: AST Match + resolved subject type -> ResolvedMatch.
//
// Reads typecheck-supplied `resolved_type` from each arm's last expression,
// applying the surrounding function's monomorphization substitution. Decides
// the result-type strategy from typecheck data alone; no LLVM emission
// happens here.
// ---------------------------------------------------------------------------

/// Lowers a match expression to a `ResolvedMatch`. The subject type is
/// passed in by `compile_match` (resolved via `resolve_subject_ty`); each
/// pattern is resolved via [`lower_pattern`] and the result-type strategy is
/// decided via [`lower_result_ty`].
fn lower_match(
    compiler: &Compiler<'_>,
    subject_ty: &Type,
    arms: &[MatchArm],
) -> Result<ResolvedMatch, String> {
    let mut patterns = Vec::with_capacity(arms.len());
    for arm in arms {
        patterns.push(lower_pattern(compiler, &arm.pattern, subject_ty)?);
    }

    let result_ty = lower_result_ty(compiler, arms);

    Ok(ResolvedMatch {
        subject_ty: subject_ty.clone(),
        patterns,
        result_ty,
    })
}

/// Decides the result-type strategy for a match expression from arm-body
/// typecheck data plus the surrounding function's union return-type hint.
///
/// "All arm types equal" -> [`ResolvedMatchType::Direct`]. "Arm types differ
/// but every value-producing arm is a member of the hinted union, and the
/// union itself is monomorphized" -> [`ResolvedMatchType::UnionWrap`].
/// Everything else falls back to `Direct` with the first contributing arm's
/// type; if emission later observes mismatched LLVM types under that
/// fallback, it errors.
fn lower_result_ty(compiler: &Compiler<'_>, arms: &[MatchArm]) -> ResolvedMatchType {
    let arm_types: Vec<Type> = arms
        .iter()
        .filter_map(|a| arm_value_type(a, compiler))
        .collect();

    if arm_types.is_empty() {
        return ResolvedMatchType::Direct { ty: Type::Unknown };
    }

    let first_mangled = mangle_type(&arm_types[0]);
    let all_eq = arm_types.iter().all(|t| mangle_type(t) == first_mangled);
    if all_eq {
        return ResolvedMatchType::Direct {
            ty: arm_types[0].clone(),
        };
    }

    if let Some(Type::Union(members)) = &compiler.fn_state.return_type_hint {
        let target = Type::Union(members.clone());
        let target_mangled = mangle_type(&target);
        let all_members = arm_types.iter().all(|t| {
            matches!(t, Type::Union(_)) || members.iter().any(|m| mangle_type(m) == mangle_type(t))
        });
        if all_members && compiler.types.contains_monomorphized(&target_mangled) {
            return ResolvedMatchType::UnionWrap { target };
        }
    }

    ResolvedMatchType::Direct {
        ty: arm_types[0].clone(),
    }
}

/// The Expo type the arm body would yield to the match's phi, derived from
/// the last statement's expression. Returns `None` for arms whose last
/// statement is not an `Expr` (e.g. ends in a `let`); those arms contribute
/// no value to the phi at runtime.
fn arm_value_type(arm: &MatchArm, compiler: &Compiler<'_>) -> Option<Type> {
    let Statement::Expr(last) = arm.body.last()? else {
        return None;
    };
    last.resolved_type
        .as_ref()
        .map(|t| compiler.monomorphize_type(t))
}

// ---------------------------------------------------------------------------
// Emission: ResolvedMatch + AST arms -> LLVM IR.
//
// Mechanically scaffolds blocks, evaluates the subject, emits each pattern
// (via `emit_pattern`) plus its guard, compiles the arm body, and assembles
// the result phi using the lowered strategy. No strategy decision happens
// here; if the LLVM phi types disagree with `result_ty`, emission errors.
// ---------------------------------------------------------------------------

fn emit_match<'ctx>(
    compiler: &mut Compiler<'ctx>,
    resolved: &ResolvedMatch,
    subject_val: BasicValueEnum<'ctx>,
    arms: &[MatchArm],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let subject_alloca = compiler
        .builder
        .build_alloca(subject_val.get_type(), "match_subject")
        .unwrap();
    compiler
        .builder
        .build_store(subject_alloca, subject_val)
        .unwrap();

    let merge_bb = compiler.context.append_basic_block(function, "match_end");
    let fallthrough_bb = compiler.context.append_basic_block(function, "match_none");

    let mut pending_arms: Vec<(BasicValueEnum<'ctx>, Type, BasicBlock<'ctx>)> = Vec::new();
    let mut needs_branch: Vec<BasicBlock<'ctx>> = Vec::new();

    for (i, (arm, resolved_pattern)) in arms.iter().zip(resolved.patterns.iter()).enumerate() {
        let body_bb = compiler
            .context
            .append_basic_block(function, &format!("match_body_{i}"));
        let next_bb = if i + 1 < arms.len() {
            compiler
                .context
                .append_basic_block(function, &format!("match_test_{}", i + 1))
        } else {
            fallthrough_bb
        };

        let saved_vars = compiler.fn_state.variables.clone();

        let condition = emit_pattern(compiler, resolved_pattern, subject_alloca, function)?;

        let final_cond = if let Some(guard) = &arm.guard {
            let guard_val = compile_expr(compiler, guard, function)?
                .ok_or("match guard produced no value")?
                .value;
            compiler
                .builder
                .build_and(condition, guard_val.into_int_value(), "guard_and")
                .unwrap()
        } else {
            condition
        };

        compiler
            .builder
            .build_conditional_branch(final_cond, body_bb, next_bb)
            .unwrap();

        compiler.builder.position_at_end(body_bb);
        let arm_tv = compile_body_as_value(compiler, &arm.body, function)?;
        let arm_terminated = compiler.current_block_terminated();
        let arm_end_bb = compiler.builder.get_insert_block().unwrap();
        if !arm_terminated {
            if let Some(tv) = arm_tv {
                pending_arms.push((tv.value, tv.expo_type, arm_end_bb));
            } else {
                needs_branch.push(arm_end_bb);
            }
        }

        compiler.fn_state.variables = saved_vars;
        compiler.builder.position_at_end(next_bb);
    }

    // Structural Void: zero arms produced a value (all terminated or all
    // ended in non-Expr statements). Emission has nothing to phi.
    if pending_arms.is_empty() {
        for bb in &needs_branch {
            compiler.builder.position_at_end(*bb);
            compiler
                .builder
                .build_unconditional_branch(merge_bb)
                .unwrap();
        }
        compiler.builder.position_at_end(fallthrough_bb);
        compiler
            .builder
            .build_unconditional_branch(merge_bb)
            .unwrap();
        compiler.builder.position_at_end(merge_bb);
        return Ok(None);
    }

    // Apply the lowered strategy to each value-producing arm and branch to
    // merge. UnionWrap may fail if the lowered union member resolution is
    // wrong -- the `?` surfaces that as an emission error.
    let mut incoming: Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> = Vec::new();
    for (val, ty, bb) in &pending_arms {
        compiler.builder.position_at_end(*bb);
        let final_val = match &resolved.result_ty {
            ResolvedMatchType::UnionWrap { target } => {
                if matches!(ty, Type::Union(_)) {
                    *val
                } else {
                    crate::stmt::compile_union_wrap(compiler, *val, ty, target)?
                }
            }
            ResolvedMatchType::Direct { .. } => *val,
        };
        compiler
            .builder
            .build_unconditional_branch(merge_bb)
            .unwrap();
        let end_bb = compiler.builder.get_insert_block().unwrap();
        incoming.push((final_val, end_bb));
    }

    for bb in &needs_branch {
        compiler.builder.position_at_end(*bb);
        compiler
            .builder
            .build_unconditional_branch(merge_bb)
            .unwrap();
    }
    compiler.builder.position_at_end(fallthrough_bb);
    compiler
        .builder
        .build_unconditional_branch(merge_bb)
        .unwrap();

    compiler.builder.position_at_end(merge_bb);

    // If any reachable arm produced no value while others did, we have no
    // unified phi shape -- this is a structural emission outcome (not a
    // strategy disagreement), so return Ok(None) the same as today.
    if !needs_branch.is_empty() {
        return Ok(None);
    }

    let first_ty = incoming[0].0.get_type();
    if !incoming.iter().all(|(v, _)| v.get_type() == first_ty) {
        return Err(format!(
            "match arms produced incompatible LLVM types under lowered strategy `{}`",
            describe_match_strategy(&resolved.result_ty),
        ));
    }

    let undef = first_ty.const_zero();
    let phi = compiler.builder.build_phi(first_ty, "matchval").unwrap();
    for (v, bb) in &incoming {
        phi.add_incoming(&[(v, *bb)]);
    }
    phi.add_incoming(&[(&undef, fallthrough_bb)]);

    let result_type = match &resolved.result_ty {
        ResolvedMatchType::UnionWrap { target } => target.clone(),
        ResolvedMatchType::Direct { ty } if !matches!(ty, Type::Unknown) => ty.clone(),
        // Fallback: lowering had no typecheck-derived result type (e.g. an
        // arm body whose `resolved_type` didn't propagate into the cached
        // impl-AST clone). Trust the post-emit Expo type of the first
        // value-producing arm instead. Once the AST-clone story is fixed
        // this branch can go away.
        ResolvedMatchType::Direct { .. } => pending_arms[0].1.clone(),
    };
    Ok(Some(TypedValue::new(phi.as_basic_value(), result_type)))
}

fn describe_match_strategy(strategy: &ResolvedMatchType) -> String {
    match strategy {
        ResolvedMatchType::Direct { ty } => format!("Direct({})", ty.display()),
        ResolvedMatchType::UnionWrap { target } => format!("UnionWrap({})", target.display()),
    }
}

/// Compiles a match pattern into a boolean condition. Bindings introduced by
/// the pattern are inserted into the compiler's variable scope.
///
/// This is a thin shim over [`lower_pattern`] + [`emit_pattern`]. Lowering
/// performs all name resolution and shape lookups; emission performs only
/// LLVM builder calls.
pub(crate) fn compile_pattern<'ctx>(
    compiler: &mut Compiler<'ctx>,
    pattern: &Pattern,
    subject_ptr: PointerValue<'ctx>,
    subject_type: &Type,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let resolved = lower_pattern(compiler, pattern, subject_type)?;
    emit_pattern(compiler, &resolved, subject_ptr, function)
}

// ---------------------------------------------------------------------------
// Lowering: AST Pattern + subject Type -> ResolvedPattern.
//
// Every helper in this region may consult `compiler.types` and
// `compiler.type_ctx`. None of them touches the LLVM builder.
// ---------------------------------------------------------------------------

/// Resolves an AST pattern against the subject's Expo type, producing a
/// `ResolvedPattern` whose enum keys, tags, field indices, and variant shapes
/// have all been validated against the type registry.
fn lower_pattern(
    compiler: &Compiler<'_>,
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
            let enum_key = resolve_enum_key_from_path(compiler, type_path, subject_type)?;
            let tag = lookup_variant_tag(compiler, &enum_key, variant)?;
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
            let enum_key = resolve_enum_key_from_path(compiler, type_path, subject_type)?;
            let tag = lookup_variant_tag(compiler, &enum_key, variant)?;
            let elements = lower_tuple_elements(compiler, &enum_key, variant, elements)?;
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
            let enum_key = resolve_enum_key_from_path(compiler, type_path, subject_type)?;
            let tag = lookup_variant_tag(compiler, &enum_key, variant)?;
            let fields = lower_struct_fields(compiler, &enum_key, variant, fields)?;
            Ok(ResolvedPattern::EnumStruct {
                enum_key,
                variant: variant.clone(),
                tag,
                fields,
            })
        }

        Pattern::Constructor { name, elements, .. } => {
            let enum_key = resolve_enum_key_from_constructor(compiler, name, subject_type)?;
            let tag = lookup_variant_tag(compiler, &enum_key, name)?;
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
                let elements = lower_tuple_elements(compiler, &enum_key, name, elements)?;
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
            let resolved = compiler.resolve_type_expr(type_expr);
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
                let tag = lookup_variant_tag(compiler, &union_mangled, &member_mangled)?;
                Ok(ResolvedPattern::UnionMember {
                    union_mangled,
                    member_mangled,
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
                subs.push(lower_pattern(compiler, p, subject_type)?);
            }
            Ok(ResolvedPattern::Or(subs))
        }
    }
}

fn lower_literal(lit: &Literal) -> Result<ResolvedLiteral, String> {
    match lit {
        Literal::Int(s) => parse_int_literal(s).map(ResolvedLiteral::Int),
        Literal::Float(s) => s
            .parse::<f64>()
            .map(ResolvedLiteral::Float)
            .map_err(|_| format!("invalid float: {s}")),
        Literal::Bool(b) => Ok(ResolvedLiteral::Bool(*b)),
        Literal::String(s) => Ok(ResolvedLiteral::String(s.clone())),
        Literal::Unit => Err("unsupported literal in match pattern".to_string()),
    }
}

fn lower_tuple_elements(
    compiler: &Compiler<'_>,
    enum_key: &str,
    variant: &str,
    elements: &[Pattern],
) -> Result<Vec<(Type, ResolvedPattern)>, String> {
    let field_types = get_tuple_variant_types(compiler, enum_key, variant)?;
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
        let sub_resolved = lower_pattern(compiler, sub, &inner)?;
        out.push((ft.clone(), sub_resolved));
    }
    Ok(out)
}

fn lower_struct_fields(
    compiler: &Compiler<'_>,
    enum_key: &str,
    variant: &str,
    fields: &[FieldPattern],
) -> Result<Vec<ResolvedFieldPattern>, String> {
    let expected = get_struct_variant_fields(compiler, enum_key, variant)?;
    let mut out = Vec::with_capacity(fields.len());
    for fp in fields {
        let (idx, (_, field_type)) = expected
            .iter()
            .enumerate()
            .find(|(_, (n, _))| *n == fp.name)
            .ok_or_else(|| format!("unknown field `{}` in {enum_key}.{variant}", fp.name))?;
        let inner_ty = unwrap_indirect(field_type);
        let sub = match &fp.pattern {
            Some(p) => Some(lower_pattern(compiler, p, inner_ty)?),
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

/// Resolves the canonical TypeRegistry key for an enum referenced via an
/// AST type path (`Color`, `alpha.Status`, generic-args-bearing `Option<T>`
/// already monomorphized in the subject type, etc.).
fn resolve_enum_key_from_path(
    compiler: &Compiler<'_>,
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
            if let Some((base, _)) = crate::generics::try_parse_mangled_name(name, compiler)
                && compiler.type_ctx.is_enum(&base)
            {
                Ok(name.clone())
            } else if identifier.package != expo_ast::identifier::Package::Unresolved {
                Ok(identifier.qualified_name())
            } else if !type_path.is_empty() {
                resolve_enum_key_from_joined(compiler, &type_path.join("."), subject_type)
            } else {
                Err("cannot determine enum name for pattern".to_string())
            }
        }
        _ if !type_path.is_empty() => {
            resolve_enum_key_from_joined(compiler, &type_path.join("."), subject_type)
        }
        _ => Err("cannot determine enum name for pattern".to_string()),
    }
}

fn resolve_enum_key_from_joined(
    compiler: &Compiler<'_>,
    joined: &str,
    subject_type: &Type,
) -> Result<String, String> {
    if let Some(id) = compiler.resolve_name_current(joined) {
        let qualified = id.qualified_name();
        if compiler.types.get_concrete(id).is_some()
            || compiler.types.contains_monomorphized(&qualified)
            || compiler.types.mono_enum_variants.contains_key(&qualified)
        {
            return Ok(qualified);
        }
    }
    if compiler
        .types
        .get_concrete(&TypeIdentifier::from_qualified_name(joined))
        .is_some()
        || compiler.types.contains_monomorphized(joined)
        || compiler.types.mono_enum_variants.contains_key(joined)
    {
        return Ok(joined.to_string());
    }
    Err(format!(
        "cannot resolve enum name from pattern `{joined}` for match subject type `{}`",
        subject_type.display()
    ))
}

/// Resolves the canonical TypeRegistry key for a shorthand constructor
/// pattern (`Some(x)`, `Ok(_)`) -- where the variant name is given without
/// an enum-name qualifier.
fn resolve_enum_key_from_constructor(
    compiler: &Compiler<'_>,
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
        if let Some((base, _)) = crate::generics::try_parse_mangled_name(name, compiler)
            && compiler.type_ctx.is_enum(&base)
        {
            return Ok(name.clone());
        }
        if identifier.package != expo_ast::identifier::Package::Unresolved
            && compiler
                .type_ctx
                .get_type(identifier)
                .is_some_and(|ti| ti.is_enum())
        {
            return Ok(identifier.qualified_name());
        }
        if compiler.type_ctx.is_enum(name) {
            return Ok(name.clone());
        }
    }
    if let Type::Union(members) = subject_type {
        let member_mangled = mangle_type(&named(variant_name));
        if members.iter().any(|m| mangle_type(m) == member_mangled) {
            return Ok(mangle_type(subject_type));
        }
    }
    for (enum_name, info) in compiler
        .type_ctx
        .types
        .iter()
        .filter(|(_, ti)| ti.is_enum())
    {
        if info
            .variants()
            .is_some_and(|vs| vs.iter().any(|v| v.name == variant_name))
        {
            return Ok(enum_name.name.clone());
        }
    }
    Err(format!("no enum found with variant `{variant_name}`"))
}

fn lookup_variant_tag(
    compiler: &Compiler<'_>,
    enum_key: &str,
    variant: &str,
) -> Result<u8, String> {
    compiler
        .types
        .get_variant_tag(enum_key, variant)
        .ok_or_else(|| format!("unknown variant: {enum_key}.{variant}"))
}

fn get_struct_variant_fields(
    compiler: &Compiler<'_>,
    enum_key: &str,
    variant: &str,
) -> Result<Vec<(String, Type)>, String> {
    let data = lookup_variant_data(compiler, enum_key, variant)?;
    match data {
        VariantData::Struct(fields) => Ok(fields),
        _ => Err(format!("{enum_key}.{variant} is not a struct variant")),
    }
}

fn get_tuple_variant_types(
    compiler: &Compiler<'_>,
    enum_key: &str,
    variant: &str,
) -> Result<Vec<Type>, String> {
    let data = lookup_variant_data(compiler, enum_key, variant)?;
    match data {
        VariantData::Tuple(types) => Ok(types),
        _ => Err(format!("{enum_key}.{variant} is not a tuple variant")),
    }
}

// ---------------------------------------------------------------------------
// Emission: ResolvedPattern -> LLVM IR.
//
// Functions in this region only call `compiler.builder`,
// `compiler.context`, and `compiler.fn_state.variables`. Type-registry
// touches are limited to deterministic `Type` -> `BasicTypeEnum`
// translations (`to_llvm_type`, `lookup_enum_struct_type`,
// `get_variant_payload_type`) for keys that lowering already validated.
// They never perform name resolution or fallback chains.
// ---------------------------------------------------------------------------

fn emit_pattern<'ctx>(
    compiler: &mut Compiler<'ctx>,
    resolved: &ResolvedPattern,
    subject_ptr: PointerValue<'ctx>,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let true_val = compiler.context.bool_type().const_int(1, false);

    match resolved {
        ResolvedPattern::AlwaysMatch => Ok(true_val),

        ResolvedPattern::Bind {
            name,
            ty,
            strict_llvm,
        } => {
            emit_bind(compiler, name, ty, *strict_llvm, subject_ptr)?;
            Ok(true_val)
        }

        ResolvedPattern::LiteralEq { lit, subject_ty } => {
            let llvm_ty = to_llvm_type(subject_ty, compiler.context, &compiler.types)
                .ok_or("cannot load subject for literal comparison")?;
            let subject_val = compiler
                .builder
                .build_load(llvm_ty, subject_ptr, "lit_subj")
                .unwrap();
            let lit_val = emit_literal_const(compiler, lit);
            match_values(compiler, &subject_val, &lit_val)
        }

        ResolvedPattern::EnumUnit { enum_key, tag, .. } => {
            // Note: a unit variant has no payload. There is no path from this
            // arm to `get_payload_ptr`; the previously-deferred GEPIndex panic
            // (payload GEP at index 1 on a tag-only enum) is unreachable here.
            emit_tag_check(compiler, subject_ptr, enum_key, *tag)
        }

        ResolvedPattern::EnumTuple {
            enum_key,
            variant,
            tag,
            elements,
        } => {
            let mut result = emit_tag_check(compiler, subject_ptr, enum_key, *tag)?;
            let (payload_type, payload_ptr) =
                get_payload_ptr(compiler, subject_ptr, enum_key, variant)?;
            for (i, (field_ty, sub)) in elements.iter().enumerate() {
                let inner = unwrap_indirect(field_ty);
                // Align with monomorphized enum payloads: ZST fields use an
                // i8 placeholder when `to_llvm_type` is `None` (e.g. `()`),
                // so LLVM layout and pattern loads stay in sync.
                let inner_llvm_ty = to_llvm_type(inner, compiler.context, &compiler.types)
                    .unwrap_or_else(|| compiler.context.i8_type().into());
                let field_ptr = compiler
                    .builder
                    .build_struct_gep(payload_type, payload_ptr, i as u32, &format!("tp{i}"))
                    .unwrap();
                let field_val =
                    load_maybe_indirect(compiler, field_ptr, field_ty, &format!("tp{i}_val"));
                let field_alloca = compiler
                    .builder
                    .build_alloca(inner_llvm_ty, &format!("tp{i}_tmp"))
                    .unwrap();
                compiler
                    .builder
                    .build_store(field_alloca, field_val)
                    .unwrap();
                let sub_result = emit_pattern(compiler, sub, field_alloca, function)?;
                result = compiler
                    .builder
                    .build_and(result, sub_result, &format!("tp{i}_and"))
                    .unwrap();
            }
            Ok(result)
        }

        ResolvedPattern::EnumStruct {
            enum_key,
            variant,
            tag,
            fields,
        } => {
            let mut result = emit_tag_check(compiler, subject_ptr, enum_key, *tag)?;
            let (payload_type, payload_ptr) =
                get_payload_ptr(compiler, subject_ptr, enum_key, variant)?;
            for fp in fields {
                let inner_ty = unwrap_indirect(&fp.field_type);
                let inner_llvm_ty = to_llvm_type(inner_ty, compiler.context, &compiler.types)
                    .ok_or_else(|| format!("unsupported field type for `{}`", fp.name))?;
                let field_ptr = compiler
                    .builder
                    .build_struct_gep(payload_type, payload_ptr, fp.field_index, &fp.name)
                    .unwrap();
                let field_val = load_maybe_indirect(
                    compiler,
                    field_ptr,
                    &fp.field_type,
                    &format!("{}_val", fp.name),
                );
                let field_alloca = compiler
                    .builder
                    .build_alloca(inner_llvm_ty, &format!("{}_tmp", fp.name))
                    .unwrap();
                compiler
                    .builder
                    .build_store(field_alloca, field_val)
                    .unwrap();

                if let Some(sub) = &fp.sub {
                    let sub_result = emit_pattern(compiler, sub, field_alloca, function)?;
                    result = compiler
                        .builder
                        .build_and(result, sub_result, &format!("{}_and", fp.name))
                        .unwrap();
                } else {
                    compiler.fn_state.variables.insert(
                        fp.name.clone(),
                        (field_alloca, inner_ty.clone(), Ownership::Unowned),
                    );
                }
            }
            Ok(result)
        }

        ResolvedPattern::UnionMember {
            union_mangled,
            member_mangled,
            tag,
            member_ty,
            bind_name,
        } => {
            let result = emit_tag_check(compiler, subject_ptr, union_mangled, *tag)?;
            let (_payload_type, payload_ptr) =
                get_payload_ptr(compiler, subject_ptr, union_mangled, member_mangled)?;
            let llvm_ty =
                to_llvm_type(member_ty, compiler.context, &compiler.types).ok_or_else(|| {
                    format!("unsupported type in typed binding: {}", member_ty.display())
                })?;
            let val = compiler
                .builder
                .build_load(llvm_ty, payload_ptr, bind_name)
                .unwrap();
            let alloca = compiler.builder.build_alloca(llvm_ty, bind_name).unwrap();
            compiler.builder.build_store(alloca, val).unwrap();
            compiler.fn_state.variables.insert(
                bind_name.clone(),
                (alloca, member_ty.clone(), Ownership::Unowned),
            );
            Ok(result)
        }

        ResolvedPattern::Or(subs) => {
            let mut result = compiler.context.bool_type().const_int(0, false);
            for sub in subs {
                let cond = emit_pattern(compiler, sub, subject_ptr, function)?;
                result = compiler.builder.build_or(result, cond, "or_pat").unwrap();
            }
            Ok(result)
        }

        ResolvedPattern::Binary { segments } => {
            emit_binary_pattern(compiler, segments, subject_ptr, function)
        }
    }
}

fn emit_bind<'ctx>(
    compiler: &mut Compiler<'ctx>,
    name: &str,
    ty: &Type,
    strict_llvm: bool,
    subject_ptr: PointerValue<'ctx>,
) -> Result<(), String> {
    let llvm_ty = if strict_llvm {
        to_llvm_type(ty, compiler.context, &compiler.types)
            .ok_or_else(|| format!("unsupported type in typed binding: {}", ty.display()))?
    } else {
        to_llvm_type(ty, compiler.context, &compiler.types)
            .unwrap_or_else(|| compiler.context.i8_type().into())
    };
    let val = compiler
        .builder
        .build_load(llvm_ty, subject_ptr, name)
        .unwrap();
    let alloca = compiler.builder.build_alloca(llvm_ty, name).unwrap();
    compiler.builder.build_store(alloca, val).unwrap();
    compiler
        .fn_state
        .variables
        .insert(name.to_string(), (alloca, ty.clone(), Ownership::Unowned));
    Ok(())
}

fn emit_literal_const<'ctx>(
    compiler: &Compiler<'ctx>,
    lit: &ResolvedLiteral,
) -> BasicValueEnum<'ctx> {
    match lit {
        ResolvedLiteral::Int(v) => compiler
            .context
            .i64_type()
            .const_int(*v as u64, true)
            .into(),
        ResolvedLiteral::Float(v) => compiler.context.f64_type().const_float(*v).into(),
        ResolvedLiteral::Bool(b) => compiler
            .context
            .bool_type()
            .const_int(if *b { 1 } else { 0 }, false)
            .into(),
        ResolvedLiteral::String(s) => compiler
            .builder
            .build_global_string_ptr(s, "str_pat")
            .unwrap()
            .as_pointer_value()
            .into(),
    }
}

fn emit_tag_check<'ctx>(
    compiler: &mut Compiler<'ctx>,
    subject_ptr: PointerValue<'ctx>,
    enum_key: &str,
    tag: u8,
) -> Result<IntValue<'ctx>, String> {
    let enum_type = lookup_enum_struct_type(compiler, enum_key)?;
    let tag_ptr = compiler
        .builder
        .build_struct_gep(enum_type, subject_ptr, 0, "tag_ptr")
        .unwrap();
    let tag_val = compiler
        .builder
        .build_load(compiler.context.i8_type(), tag_ptr, "tag")
        .unwrap()
        .into_int_value();
    let expected = compiler.context.i8_type().const_int(tag as u64, false);
    Ok(compiler
        .builder
        .build_int_compare(IntPredicate::EQ, tag_val, expected, "tag_eq")
        .unwrap())
}

fn lookup_enum_struct_type<'ctx>(
    compiler: &Compiler<'ctx>,
    enum_key: &str,
) -> Result<StructType<'ctx>, String> {
    compiler
        .types
        .get_concrete(&TypeIdentifier::from_qualified_name(enum_key))
        .or_else(|| compiler.types.get_monomorphized(enum_key))
        .ok_or_else(|| format!("unknown enum: {enum_key}"))
}

fn emit_binary_pattern<'ctx>(
    compiler: &mut Compiler<'ctx>,
    segments: &[BinarySegment],
    subject_ptr: PointerValue<'ctx>,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    compile_binary_pattern(compiler, segments, subject_ptr, function)
}

// ---------------------------------------------------------------------------
// Shared helpers used by both lowering and emission, plus by other codegen
// modules (`enums.rs` consumes `get_payload_ptr`, `lookup_variant_data`,
// `match_values` for enum equality compilation).
// ---------------------------------------------------------------------------

/// Resolved payload metadata for an enum variant.
struct ResolvedPayloadInfo<'ctx> {
    enum_type: StructType<'ctx>,
    payload_type: StructType<'ctx>,
}

/// Looks up the payload and enum LLVM types for a variant from the type registry.
fn resolve_payload_info<'ctx>(
    compiler: &Compiler<'ctx>,
    enum_name: &str,
    variant: &str,
) -> Result<ResolvedPayloadInfo<'ctx>, String> {
    let payload_type = compiler
        .types
        .get_variant_payload_type(enum_name, variant)
        .ok_or_else(|| format!("no payload type for {enum_name}.{variant}"))?;
    let enum_type = lookup_enum_struct_type(compiler, enum_name)?;
    Ok(ResolvedPayloadInfo {
        enum_type,
        payload_type,
    })
}

pub(crate) fn get_payload_ptr<'ctx>(
    compiler: &mut Compiler<'ctx>,
    subject_ptr: PointerValue<'ctx>,
    enum_name: &str,
    variant: &str,
) -> Result<(StructType<'ctx>, PointerValue<'ctx>), String> {
    let resolved = resolve_payload_info(compiler, enum_name, variant)?;
    let payload_ptr = compiler
        .builder
        .build_struct_gep(resolved.enum_type, subject_ptr, 1, "payload_ptr")
        .unwrap();
    Ok((resolved.payload_type, payload_ptr))
}

pub(crate) fn lookup_variant_data(
    compiler: &Compiler<'_>,
    enum_name: &str,
    variant: &str,
) -> Result<VariantData, String> {
    if let Some(ti) = compiler.type_ctx.find_type(enum_name)
        && let Some(vs) = ti.variants()
        && let Some(vi) = vs.iter().find(|v| v.name == variant)
    {
        return Ok(vi.data.clone());
    }
    if let Some(variants) = compiler.types.mono_enum_variants.get(enum_name)
        && let Some((_, data)) = variants.iter().find(|(n, _)| n == variant)
    {
        return Ok(data.clone());
    }
    Err(format!("variant not found: {enum_name}.{variant}"))
}

pub(crate) fn match_values<'ctx>(
    compiler: &mut Compiler<'ctx>,
    subject: &BasicValueEnum<'ctx>,
    lit: &BasicValueEnum<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    if subject.is_int_value() && lit.is_int_value() {
        let subj_iv = subject.into_int_value();
        let mut lit_iv = lit.into_int_value();
        let subj_bits = subj_iv.get_type().get_bit_width();
        let lit_bits = lit_iv.get_type().get_bit_width();
        if subj_bits != lit_bits {
            let target_ty = compiler.context.custom_width_int_type(subj_bits);
            lit_iv = if subj_bits < lit_bits {
                compiler
                    .builder
                    .build_int_truncate(lit_iv, target_ty, "lit_trunc")
                    .unwrap()
            } else {
                compiler
                    .builder
                    .build_int_s_extend(lit_iv, target_ty, "lit_sext")
                    .unwrap()
            };
        }
        Ok(compiler
            .builder
            .build_int_compare(IntPredicate::EQ, subj_iv, lit_iv, "lit_eq")
            .unwrap())
    } else if subject.is_float_value() && lit.is_float_value() {
        Ok(compiler
            .builder
            .build_float_compare(
                FloatPredicate::OEQ,
                subject.into_float_value(),
                lit.into_float_value(),
                "lit_feq",
            )
            .unwrap())
    } else if subject.is_pointer_value() && lit.is_pointer_value() {
        let strcmp = *compiler
            .functions
            .get("strcmp")
            .ok_or("strcmp not declared")?;
        let cmp_result = compiler
            .call(
                strcmp,
                &[
                    subject.into_pointer_value().into(),
                    lit.into_pointer_value().into(),
                ],
                "strcmp_result",
            )
            .ok_or("strcmp did not return a value")?
            .into_int_value();
        let zero = compiler.context.i32_type().const_int(0, false);
        Ok(compiler
            .builder
            .build_int_compare(IntPredicate::EQ, cmp_result, zero, "str_eq")
            .unwrap())
    } else {
        Err("unsupported literal pattern comparison".to_string())
    }
}
