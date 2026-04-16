//! Pattern matching compilation: `match` expressions and pattern-to-boolean
//! lowering for all pattern variants (bindings, literals, enum variants, typed
//! bindings, constructors).

use crate::binary::patterns::compile_binary_pattern;
use crate::drop::Ownership;
use expo_ast::ast::{Expr, ExprKind, FieldPattern, Literal, MatchArm, Pattern};
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{
    Type, TypeIdentifier, mangle_name, mangle_type, named, unwrap_indirect,
};
use inkwell::FloatPredicate;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::compiler::TypeRegistry;

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::compile_expr;
use crate::structs::load_maybe_indirect;
use crate::types::to_llvm_type;
use crate::util::parse_int_literal;

use super::compile_body_as_value;

enum MatchResultStrategy {
    Direct,
    UnionWrap { target: Type },
    Void,
}

fn resolve_match_result<'ctx>(
    pending_arms: &[(BasicValueEnum<'ctx>, Type, BasicBlock<'ctx>)],
    return_type_hint: &Option<Type>,
    types: &TypeRegistry<'ctx>,
) -> MatchResultStrategy {
    if pending_arms.is_empty() {
        return MatchResultStrategy::Void;
    }

    let types_uniform = pending_arms
        .iter()
        .all(|(v, _, _)| v.get_type() == pending_arms[0].0.get_type());

    if types_uniform {
        return MatchResultStrategy::Direct;
    }

    if let Some(Type::Union(members)) = return_type_hint {
        let target = Type::Union(members.clone());
        let target_mangled = mangle_type(&target);
        let all_members = pending_arms.iter().all(|(_, ty, _)| {
            matches!(ty, Type::Union(_))
                || members.iter().any(|m| mangle_type(m) == mangle_type(ty))
        });
        if all_members && types.contains_monomorphized(&target_mangled) {
            return MatchResultStrategy::UnionWrap { target };
        }
    }

    MatchResultStrategy::Direct
}

/// Compiles a `match` expression. Patterns are tested sequentially; the first
/// matching arm executes. Bindings introduced by patterns are scoped to their
/// arm. Returns a phi value when all arms produce a value of the same type.
pub fn compile_match<'ctx>(
    compiler: &mut Compiler<'ctx>,
    subject: &Expr,
    arms: &[MatchArm],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let subject_tv =
        compile_expr(compiler, subject, function)?.ok_or("match subject produced no value")?;
    let subject_val = subject_tv.value;

    let subject_type = if subject_tv.expo_type != Type::Unknown {
        subject_tv.expo_type
    } else {
        infer_subject_type(compiler, subject)
    };

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
    let mut arm_expo_type: Option<Type> = None;
    let mut reachable_arm_count = 0usize;
    let mut pending_arms: Vec<(BasicValueEnum<'ctx>, Type, BasicBlock<'ctx>)> = Vec::new();
    let mut needs_branch: Vec<BasicBlock<'ctx>> = Vec::new();

    for (i, arm) in arms.iter().enumerate() {
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

        let condition = compile_pattern(
            compiler,
            &arm.pattern,
            subject_alloca,
            &subject_type,
            function,
        )?;

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
            reachable_arm_count += 1;
            if let Some(tv) = arm_tv {
                if arm_expo_type.is_none() {
                    arm_expo_type = Some(tv.expo_type.clone());
                }
                pending_arms.push((tv.value, tv.expo_type, arm_end_bb));
            } else {
                needs_branch.push(arm_end_bb);
            }
        }

        compiler.fn_state.variables = saved_vars;
        compiler.builder.position_at_end(next_bb);
    }

    let strategy = resolve_match_result(
        &pending_arms,
        &compiler.fn_state.return_type_hint,
        &compiler.types,
    );

    if matches!(strategy, MatchResultStrategy::Void) {
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

    let mut incoming: Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> = Vec::new();

    for (val, ty, bb) in &pending_arms {
        compiler.builder.position_at_end(*bb);
        let final_val = match &strategy {
            MatchResultStrategy::UnionWrap { target } => {
                if matches!(ty, Type::Union(_)) {
                    *val
                } else {
                    crate::stmt::compile_union_wrap(compiler, *val, ty, target)?
                }
            }
            _ => *val,
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

    if !incoming.is_empty() && incoming.len() == reachable_arm_count {
        let first_ty = incoming[0].0.get_type();
        if incoming.iter().all(|(v, _)| v.get_type() == first_ty) {
            let undef = first_ty.const_zero();
            let phi = compiler.builder.build_phi(first_ty, "matchval").unwrap();
            for (v, bb) in &incoming {
                phi.add_incoming(&[(v, *bb)]);
            }
            phi.add_incoming(&[(&undef, fallthrough_bb)]);
            let result_type = match strategy {
                MatchResultStrategy::UnionWrap { target } => Some(target),
                _ => arm_expo_type,
            }
            .unwrap_or(Type::Unknown);
            return Ok(Some(TypedValue::new(phi.as_basic_value(), result_type)));
        }
    }

    Ok(None)
}

/// Infers the Expo type for a match subject from variable bindings when
/// the TypedValue carries `Type::Unknown`.
fn infer_subject_type(compiler: &Compiler, subject: &Expr) -> Type {
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

/// Recursively compiles a match pattern into a boolean condition. As a side
/// effect, binds matched variables into the compiler's variable scope.
pub(crate) fn compile_pattern<'ctx>(
    compiler: &mut Compiler<'ctx>,
    pattern: &Pattern,
    subject_ptr: PointerValue<'ctx>,
    subject_type: &Type,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let true_val = compiler.context.bool_type().const_int(1, false);

    match pattern {
        Pattern::Wildcard { .. } => Ok(true_val),

        Pattern::Binding { name, .. } => {
            let llvm_ty = to_llvm_type(subject_type, compiler.context, &compiler.types)
                .unwrap_or_else(|| compiler.context.i8_type().into());
            let val = compiler
                .builder
                .build_load(llvm_ty, subject_ptr, name)
                .unwrap();
            let alloca = compiler.builder.build_alloca(llvm_ty, name).unwrap();
            compiler.builder.build_store(alloca, val).unwrap();
            compiler.fn_state.variables.insert(
                name.clone(),
                (alloca, subject_type.clone(), Ownership::Unowned),
            );
            Ok(true_val)
        }

        Pattern::Literal { value, .. } => {
            let llvm_ty = to_llvm_type(subject_type, compiler.context, &compiler.types)
                .ok_or("cannot load subject for literal comparison")?;
            let subject_val = compiler
                .builder
                .build_load(llvm_ty, subject_ptr, "lit_subj")
                .unwrap();
            let lit_val = compile_literal_for_pattern(compiler, value)?;
            match_values(compiler, &subject_val, &lit_val)
        }

        Pattern::EnumUnit {
            type_path, variant, ..
        } => {
            let enum_name = enum_name_from_path(compiler, type_path, subject_type)?;
            compile_tag_check(compiler, subject_ptr, &enum_name, variant)
        }

        Pattern::EnumTuple {
            type_path,
            variant,
            elements,
            ..
        } => {
            let enum_name = enum_name_from_path(compiler, type_path, subject_type)?;
            let mut result = compile_tag_check(compiler, subject_ptr, &enum_name, variant)?;
            let (payload_type, payload_ptr) =
                get_payload_ptr(compiler, subject_ptr, &enum_name, variant)?;
            let field_types = get_tuple_variant_types(compiler, &enum_name, variant)?;
            result = compile_tuple_elements(
                compiler,
                elements,
                &field_types,
                payload_type,
                payload_ptr,
                result,
                function,
            )?;
            Ok(result)
        }

        Pattern::EnumStruct {
            type_path,
            variant,
            fields,
            ..
        } => {
            let enum_name = enum_name_from_path(compiler, type_path, subject_type)?;
            let mut result = compile_tag_check(compiler, subject_ptr, &enum_name, variant)?;
            let (payload_type, payload_ptr) =
                get_payload_ptr(compiler, subject_ptr, &enum_name, variant)?;
            let expected_fields = get_struct_variant_fields(compiler, &enum_name, variant)?;

            for fp in fields {
                result = compile_field_pattern(
                    compiler,
                    fp,
                    &expected_fields,
                    payload_type,
                    payload_ptr,
                    result,
                    &enum_name,
                    variant,
                    function,
                )?;
            }

            Ok(result)
        }

        Pattern::Constructor { name, elements, .. } => {
            let enum_name = find_constructor_enum(compiler, name, subject_type)?;
            let mut result = compile_tag_check(compiler, subject_ptr, &enum_name, name)?;

            if !elements.is_empty() {
                let (payload_type, payload_ptr) =
                    get_payload_ptr(compiler, subject_ptr, &enum_name, name)?;
                let field_types = get_tuple_variant_types(compiler, &enum_name, name)?;
                result = compile_tuple_elements(
                    compiler,
                    elements,
                    &field_types,
                    payload_type,
                    payload_ptr,
                    result,
                    function,
                )?;
            }

            Ok(result)
        }

        Pattern::TypedBinding {
            name, type_expr, ..
        } => {
            let resolved = compiler.resolve_type_expr(type_expr);

            if mangle_type(&resolved) == mangle_type(unwrap_indirect(subject_type)) {
                let llvm_ty = to_llvm_type(&resolved, compiler.context, &compiler.types)
                    .ok_or_else(|| {
                        format!("unsupported type in typed binding: {}", resolved.display())
                    })?;
                let val = compiler
                    .builder
                    .build_load(llvm_ty, subject_ptr, name)
                    .unwrap();
                let alloca = compiler.builder.build_alloca(llvm_ty, name).unwrap();
                compiler.builder.build_store(alloca, val).unwrap();
                compiler
                    .fn_state
                    .variables
                    .insert(name.clone(), (alloca, resolved, Ownership::Unowned));
                Ok(compiler.context.bool_type().const_int(1, false))
            } else {
                let member_mangled = mangle_type(&resolved);
                let union_mangled = mangle_type(unwrap_indirect(subject_type));

                let result =
                    compile_tag_check(compiler, subject_ptr, &union_mangled, &member_mangled)?;

                let (_payload_type, payload_ptr) =
                    get_payload_ptr(compiler, subject_ptr, &union_mangled, &member_mangled)?;
                let llvm_ty = to_llvm_type(&resolved, compiler.context, &compiler.types)
                    .ok_or_else(|| {
                        format!("unsupported type in typed binding: {}", resolved.display())
                    })?;
                let val = compiler
                    .builder
                    .build_load(llvm_ty, payload_ptr, name)
                    .unwrap();
                let alloca = compiler.builder.build_alloca(llvm_ty, name).unwrap();
                compiler.builder.build_store(alloca, val).unwrap();
                compiler
                    .fn_state
                    .variables
                    .insert(name.clone(), (alloca, resolved, Ownership::Unowned));

                Ok(result)
            }
        }

        Pattern::List { .. } => Err("list patterns not yet supported in compilation".to_string()),
        Pattern::Binary { segments, .. } => {
            compile_binary_pattern(compiler, segments, subject_ptr, function)
        }
        Pattern::Or { patterns, .. } => {
            let mut result = compiler.context.bool_type().const_int(0, false);
            for sub in patterns {
                let cond = compile_pattern(compiler, sub, subject_ptr, subject_type, function)?;
                result = compiler.builder.build_or(result, cond, "or_pat").unwrap();
            }
            Ok(result)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_field_pattern<'ctx>(
    compiler: &mut Compiler<'ctx>,
    fp: &FieldPattern,
    expected_fields: &[(String, Type)],
    payload_type: StructType<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    mut result: IntValue<'ctx>,
    enum_name: &str,
    variant: &str,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let (field_idx, (_, field_type)) = expected_fields
        .iter()
        .enumerate()
        .find(|(_, (name, _))| *name == fp.name)
        .ok_or_else(|| format!("unknown field `{}` in {enum_name}.{variant}", fp.name))?;

    let inner_ty = unwrap_indirect(field_type);
    let inner_llvm_ty = to_llvm_type(inner_ty, compiler.context, &compiler.types)
        .ok_or_else(|| format!("unsupported field type for `{}`", fp.name))?;
    let field_ptr = compiler
        .builder
        .build_struct_gep(payload_type, payload_ptr, field_idx as u32, &fp.name)
        .unwrap();
    let field_val =
        load_maybe_indirect(compiler, field_ptr, field_type, &format!("{}_val", fp.name));
    let field_alloca = compiler
        .builder
        .build_alloca(inner_llvm_ty, &format!("{}_tmp", fp.name))
        .unwrap();
    compiler
        .builder
        .build_store(field_alloca, field_val)
        .unwrap();

    if let Some(sub_pat) = &fp.pattern {
        let sub_result = compile_pattern(compiler, sub_pat, field_alloca, inner_ty, function)?;
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

    Ok(result)
}

fn compile_literal_for_pattern<'ctx>(
    compiler: &Compiler<'ctx>,
    lit: &Literal,
) -> Result<BasicValueEnum<'ctx>, String> {
    match lit {
        Literal::Int(s) => {
            let val = parse_int_literal(s)?;
            Ok(compiler
                .context
                .i64_type()
                .const_int(val as u64, true)
                .into())
        }
        Literal::Float(s) => {
            let val: f64 = s.parse().map_err(|_| format!("invalid float: {s}"))?;
            Ok(compiler.context.f64_type().const_float(val).into())
        }
        Literal::Bool(b) => Ok(compiler
            .context
            .bool_type()
            .const_int(if *b { 1 } else { 0 }, false)
            .into()),
        Literal::String(s) => {
            let global = compiler
                .builder
                .build_global_string_ptr(s, "str_pat")
                .unwrap();
            Ok(global.as_pointer_value().into())
        }
        _ => Err("unsupported literal in match pattern".to_string()),
    }
}

/// Looks up the tag value for an enum variant from the type registry.
fn resolve_variant_tag(compiler: &Compiler, enum_name: &str, variant: &str) -> Result<u8, String> {
    compiler
        .types
        .get_variant_tag(enum_name, variant)
        .ok_or_else(|| format!("unknown variant: {enum_name}.{variant}"))
}

fn compile_tag_check<'ctx>(
    compiler: &mut Compiler<'ctx>,
    subject_ptr: PointerValue<'ctx>,
    enum_name: &str,
    variant: &str,
) -> Result<IntValue<'ctx>, String> {
    let tag = resolve_variant_tag(compiler, enum_name, variant)?;
    let enum_type = compiler
        .types
        .get_concrete(&TypeIdentifier::unresolved(enum_name))
        .or_else(|| compiler.types.get_monomorphized(enum_name))
        .ok_or_else(|| format!("unknown enum: {enum_name}"))?;
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

fn compile_tuple_elements<'ctx>(
    compiler: &mut Compiler<'ctx>,
    elements: &[Pattern],
    field_types: &[Type],
    payload_type: StructType<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    mut result: IntValue<'ctx>,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    for (i, sub_pat) in elements.iter().enumerate() {
        let field_type = &field_types[i];
        let inner_ty = unwrap_indirect(field_type);
        // Align with monomorphized enum payloads: ZST fields use an i8 placeholder when
        // `to_llvm_type` is `None` (e.g. `()`), so LLVM layout and pattern loads stay in sync.
        let inner_llvm_ty = to_llvm_type(inner_ty, compiler.context, &compiler.types)
            .unwrap_or_else(|| compiler.context.i8_type().into());
        let field_ptr = compiler
            .builder
            .build_struct_gep(payload_type, payload_ptr, i as u32, &format!("tp{i}"))
            .unwrap();
        let field_val = load_maybe_indirect(compiler, field_ptr, field_type, &format!("tp{i}_val"));
        let field_alloca = compiler
            .builder
            .build_alloca(inner_llvm_ty, &format!("tp{i}_tmp"))
            .unwrap();
        compiler
            .builder
            .build_store(field_alloca, field_val)
            .unwrap();

        let sub_result = compile_pattern(compiler, sub_pat, field_alloca, inner_ty, function)?;
        result = compiler
            .builder
            .build_and(result, sub_result, &format!("tp{i}_and"))
            .unwrap();
    }
    Ok(result)
}

fn enum_name_from_path<'ctx>(
    compiler: &Compiler<'ctx>,
    type_path: &[String],
    subject_type: &Type,
) -> Result<String, String> {
    let ty = unwrap_indirect(subject_type);
    match ty {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => Ok(mangle_name(&identifier.name, type_args)),
        Type::Named { identifier, .. } => {
            let name = &identifier.name;
            if let Some((base, _)) = crate::generics::try_parse_mangled_name(name, compiler)
                && compiler.type_ctx.is_enum(&base)
            {
                Ok(name.clone())
            } else if !type_path.is_empty() {
                let joined = type_path.join(".");
                if compiler
                    .types
                    .get_concrete(&TypeIdentifier::unresolved(&joined))
                    .is_some()
                    || compiler.types.contains_monomorphized(&joined)
                    || compiler.types.mono_enum_variants.contains_key(&joined)
                {
                    Ok(joined)
                } else {
                    Err(format!(
                        "cannot resolve enum name from pattern `{joined}` for match subject type `{}`",
                        subject_type.display()
                    ))
                }
            } else {
                Err("cannot determine enum name for pattern".to_string())
            }
        }
        _ if !type_path.is_empty() => {
            let joined = type_path.join(".");
            if compiler
                .types
                .get_concrete(&TypeIdentifier::unresolved(&joined))
                .is_some()
                || compiler.types.contains_monomorphized(&joined)
                || compiler.types.mono_enum_variants.contains_key(&joined)
            {
                Ok(joined)
            } else {
                Err(format!(
                    "cannot resolve enum name from pattern `{joined}` for match subject type `{}`",
                    subject_type.display()
                ))
            }
        }
        _ => Err("cannot determine enum name for pattern".to_string()),
    }
}

fn find_constructor_enum<'ctx>(
    compiler: &Compiler<'ctx>,
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
            return Ok(mangle_name(name, type_args));
        }
        if let Some((base, _)) = crate::generics::try_parse_mangled_name(name, compiler)
            && compiler.type_ctx.is_enum(&base)
        {
            return Ok(name.clone());
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
    let enum_type = compiler
        .types
        .get_concrete(&TypeIdentifier::unresolved(enum_name))
        .or_else(|| compiler.types.get_monomorphized(enum_name))
        .ok_or_else(|| format!("unknown enum: {enum_name}"))?;
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

fn get_struct_variant_fields(
    compiler: &Compiler<'_>,
    enum_name: &str,
    variant: &str,
) -> Result<Vec<(String, Type)>, String> {
    let data = lookup_variant_data(compiler, enum_name, variant)?;
    match data {
        VariantData::Struct(fields) => Ok(fields),
        _ => Err(format!("{enum_name}.{variant} is not a struct variant")),
    }
}

fn get_tuple_variant_types(
    compiler: &Compiler<'_>,
    enum_name: &str,
    variant: &str,
) -> Result<Vec<Type>, String> {
    let data = lookup_variant_data(compiler, enum_name, variant)?;
    match data {
        VariantData::Tuple(types) => Ok(types),
        _ => Err(format!("{enum_name}.{variant} is not a tuple variant")),
    }
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
