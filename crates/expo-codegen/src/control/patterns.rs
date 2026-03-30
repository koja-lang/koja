//! Pattern matching compilation: `match` expressions and pattern-to-boolean
//! lowering for all pattern variants (bindings, literals, enum variants, typed
//! bindings, constructors).

use crate::binary::patterns::compile_binary_pattern;
use crate::drop::Ownership;
use expo_ast::ast::{Expr, FieldPattern, Literal, MatchArm, Pattern};
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{GenericKind, Type, mangle_name, mangle_type, unwrap_indirect};
use inkwell::FloatPredicate;
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::compile_expr;
use crate::structs::load_maybe_indirect;
use crate::types::to_llvm_type;

use super::compile_body_as_value;

/// Compiles a `match` expression. Patterns are tested sequentially; the first
/// matching arm executes. Bindings introduced by patterns are scoped to their
/// arm. Returns a phi value when all arms produce a value of the same type.
pub fn compile_match<'ctx>(
    c: &mut Compiler<'ctx>,
    subject: &Expr,
    arms: &[MatchArm],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let subject_tv =
        compile_expr(c, subject, function)?.ok_or("match subject produced no value")?;
    let subject_val = subject_tv.value;

    let subject_type = if subject_tv.expo_type != Type::Unknown {
        subject_tv.expo_type
    } else {
        infer_subject_type(c, subject)
    };

    let subject_alloca = c
        .builder
        .build_alloca(subject_val.get_type(), "match_subject")
        .unwrap();
    c.builder.build_store(subject_alloca, subject_val).unwrap();

    let merge_bb = c.context.append_basic_block(function, "match_end");
    let fallthrough_bb = c.context.append_basic_block(function, "match_none");
    let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
        Vec::new();
    let mut arm_expo_type: Option<Type> = None;

    let mut reachable_arm_count = 0usize;

    for (i, arm) in arms.iter().enumerate() {
        let body_bb = c
            .context
            .append_basic_block(function, &format!("match_body_{i}"));
        let next_bb = if i + 1 < arms.len() {
            c.context
                .append_basic_block(function, &format!("match_test_{}", i + 1))
        } else {
            fallthrough_bb
        };

        let saved_vars = c.fn_state.variables.clone();

        let condition = compile_pattern(c, &arm.pattern, subject_alloca, &subject_type, function)?;

        let final_cond = if let Some(guard) = &arm.guard {
            let guard_val = compile_expr(c, guard, function)?
                .ok_or("match guard produced no value")?
                .value;
            c.builder
                .build_and(condition, guard_val.into_int_value(), "guard_and")
                .unwrap()
        } else {
            condition
        };

        c.builder
            .build_conditional_branch(final_cond, body_bb, next_bb)
            .unwrap();

        c.builder.position_at_end(body_bb);
        let arm_tv = compile_body_as_value(c, &arm.body, function)?;
        let arm_terminated = c.current_block_terminated();
        if !arm_terminated {
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            reachable_arm_count += 1;
        }
        let arm_end_bb = c.builder.get_insert_block().unwrap();
        if let Some(tv) = arm_tv {
            if arm_expo_type.is_none() {
                arm_expo_type = Some(tv.expo_type.clone());
            }
            incoming.push((tv.value, arm_end_bb));
        }

        c.fn_state.variables = saved_vars;
        c.builder.position_at_end(next_bb);
    }

    c.builder.position_at_end(fallthrough_bb);
    c.builder.build_unconditional_branch(merge_bb).unwrap();

    c.builder.position_at_end(merge_bb);

    if !incoming.is_empty() && incoming.len() == reachable_arm_count {
        let first_ty = incoming[0].0.get_type();
        if incoming.iter().all(|(v, _)| v.get_type() == first_ty) {
            let undef = first_ty.const_zero();
            incoming.push((undef, fallthrough_bb));

            let phi = c.builder.build_phi(first_ty, "matchval").unwrap();
            for (v, bb) in &incoming {
                phi.add_incoming(&[(v, *bb)]);
            }
            let result_type = arm_expo_type.unwrap_or(Type::Unknown);
            return Ok(Some(TypedValue::new(phi.as_basic_value(), result_type)));
        }
    }

    Ok(None)
}

/// Infers the Expo type for a match subject from variable bindings when
/// the TypedValue carries `Type::Unknown`.
fn infer_subject_type(c: &Compiler, subject: &Expr) -> Type {
    if let Expr::Ident { name, .. } = subject
        && let Some((_, ty, _)) = c.fn_state.variables.get(name)
    {
        return ty.clone();
    }
    if matches!(subject, Expr::Self_ { .. })
        && let Some((_, ty, _)) = c.fn_state.variables.get("self")
    {
        return ty.clone();
    }
    Type::Unknown
}

/// Recursively compiles a match pattern into a boolean condition. As a side
/// effect, binds matched variables into the compiler's variable scope.
pub(crate) fn compile_pattern<'ctx>(
    c: &mut Compiler<'ctx>,
    pattern: &Pattern,
    subject_ptr: PointerValue<'ctx>,
    subject_type: &Type,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let true_val = c.context.bool_type().const_int(1, false);

    match pattern {
        Pattern::Wildcard { .. } => Ok(true_val),

        Pattern::Binding { name, .. } => {
            let llvm_ty = to_llvm_type(subject_type, c.context, &c.types.structs)
                .unwrap_or_else(|| c.context.i8_type().into());
            let val = c.builder.build_load(llvm_ty, subject_ptr, name).unwrap();
            let alloca = c.builder.build_alloca(llvm_ty, name).unwrap();
            c.builder.build_store(alloca, val).unwrap();
            c.fn_state.variables.insert(
                name.clone(),
                (alloca, subject_type.clone(), Ownership::Unowned),
            );
            Ok(true_val)
        }

        Pattern::Literal { value, .. } => {
            let llvm_ty = to_llvm_type(subject_type, c.context, &c.types.structs)
                .ok_or("cannot load subject for literal comparison")?;
            let subject_val = c
                .builder
                .build_load(llvm_ty, subject_ptr, "lit_subj")
                .unwrap();
            let lit_val = compile_literal_for_pattern(c, value)?;
            match_values(c, &subject_val, &lit_val)
        }

        Pattern::EnumUnit {
            type_path, variant, ..
        } => {
            let enum_name = enum_name_from_path(c, type_path, subject_type)?;
            compile_tag_check(c, subject_ptr, &enum_name, variant)
        }

        Pattern::EnumTuple {
            type_path,
            variant,
            elements,
            ..
        } => {
            let enum_name = enum_name_from_path(c, type_path, subject_type)?;
            let mut result = compile_tag_check(c, subject_ptr, &enum_name, variant)?;
            let (payload_type, payload_ptr) = get_payload_ptr(c, subject_ptr, &enum_name, variant)?;
            let field_types = get_tuple_variant_types(c, &enum_name, variant)?;
            result = compile_tuple_elements(
                c,
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
            let enum_name = enum_name_from_path(c, type_path, subject_type)?;
            let mut result = compile_tag_check(c, subject_ptr, &enum_name, variant)?;
            let (payload_type, payload_ptr) = get_payload_ptr(c, subject_ptr, &enum_name, variant)?;
            let expected_fields = get_struct_variant_fields(c, &enum_name, variant)?;

            for fp in fields {
                result = compile_field_pattern(
                    c,
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
            let enum_name = find_constructor_enum(c, name, subject_type)?;
            let mut result = compile_tag_check(c, subject_ptr, &enum_name, name)?;

            if !elements.is_empty() {
                let (payload_type, payload_ptr) =
                    get_payload_ptr(c, subject_ptr, &enum_name, name)?;
                let field_types = get_tuple_variant_types(c, &enum_name, name)?;
                result = compile_tuple_elements(
                    c,
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
            let resolved = c.resolve_type_expr(type_expr);

            if mangle_type(&resolved) == mangle_type(unwrap_indirect(subject_type)) {
                let llvm_ty =
                    to_llvm_type(&resolved, c.context, &c.types.structs).ok_or_else(|| {
                        format!("unsupported type in typed binding: {}", resolved.display())
                    })?;
                let val = c.builder.build_load(llvm_ty, subject_ptr, name).unwrap();
                let alloca = c.builder.build_alloca(llvm_ty, name).unwrap();
                c.builder.build_store(alloca, val).unwrap();
                c.fn_state
                    .variables
                    .insert(name.clone(), (alloca, resolved, Ownership::Unowned));
                Ok(c.context.bool_type().const_int(1, false))
            } else {
                let member_mangled = mangle_type(&resolved);
                let union_mangled = mangle_type(unwrap_indirect(subject_type));

                let result = compile_tag_check(c, subject_ptr, &union_mangled, &member_mangled)?;

                let (_payload_type, payload_ptr) =
                    get_payload_ptr(c, subject_ptr, &union_mangled, &member_mangled)?;
                let llvm_ty =
                    to_llvm_type(&resolved, c.context, &c.types.structs).ok_or_else(|| {
                        format!("unsupported type in typed binding: {}", resolved.display())
                    })?;
                let val = c.builder.build_load(llvm_ty, payload_ptr, name).unwrap();
                let alloca = c.builder.build_alloca(llvm_ty, name).unwrap();
                c.builder.build_store(alloca, val).unwrap();
                c.fn_state
                    .variables
                    .insert(name.clone(), (alloca, resolved, Ownership::Unowned));

                Ok(result)
            }
        }

        Pattern::List { .. } => Err("list patterns not yet supported in compilation".to_string()),
        Pattern::Binary { segments, .. } => {
            compile_binary_pattern(c, segments, subject_ptr, function)
        }
        Pattern::Or { patterns, .. } => {
            let mut result = c.context.bool_type().const_int(0, false);
            for sub in patterns {
                let cond = compile_pattern(c, sub, subject_ptr, subject_type, function)?;
                result = c.builder.build_or(result, cond, "or_pat").unwrap();
            }
            Ok(result)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_field_pattern<'ctx>(
    c: &mut Compiler<'ctx>,
    fp: &FieldPattern,
    expected_fields: &[(String, Type)],
    payload_type: inkwell::types::StructType<'ctx>,
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
    let inner_llvm_ty = to_llvm_type(inner_ty, c.context, &c.types.structs)
        .ok_or_else(|| format!("unsupported field type for `{}`", fp.name))?;
    let field_ptr = c
        .builder
        .build_struct_gep(payload_type, payload_ptr, field_idx as u32, &fp.name)
        .unwrap();
    let field_val = load_maybe_indirect(c, field_ptr, field_type, &format!("{}_val", fp.name));
    let field_alloca = c
        .builder
        .build_alloca(inner_llvm_ty, &format!("{}_tmp", fp.name))
        .unwrap();
    c.builder.build_store(field_alloca, field_val).unwrap();

    if let Some(sub_pat) = &fp.pattern {
        let sub_result = compile_pattern(c, sub_pat, field_alloca, inner_ty, function)?;
        result = c
            .builder
            .build_and(result, sub_result, &format!("{}_and", fp.name))
            .unwrap();
    } else {
        c.fn_state.variables.insert(
            fp.name.clone(),
            (field_alloca, inner_ty.clone(), Ownership::Unowned),
        );
    }

    Ok(result)
}

fn compile_literal_for_pattern<'ctx>(
    c: &Compiler<'ctx>,
    lit: &Literal,
) -> Result<BasicValueEnum<'ctx>, String> {
    match lit {
        Literal::Int(s) => {
            let val = crate::util::parse_int_literal(s)?;
            Ok(c.context.i64_type().const_int(val as u64, true).into())
        }
        Literal::Float(s) => {
            let val: f64 = s.parse().map_err(|_| format!("invalid float: {s}"))?;
            Ok(c.context.f64_type().const_float(val).into())
        }
        Literal::Bool(b) => Ok(c
            .context
            .bool_type()
            .const_int(if *b { 1 } else { 0 }, false)
            .into()),
        Literal::String(s) => {
            let global = c.builder.build_global_string_ptr(s, "str_pat").unwrap();
            Ok(global.as_pointer_value().into())
        }
        _ => Err("unsupported literal in match pattern".to_string()),
    }
}

fn compile_tag_check<'ctx>(
    c: &mut Compiler<'ctx>,
    subject_ptr: PointerValue<'ctx>,
    enum_name: &str,
    variant: &str,
) -> Result<IntValue<'ctx>, String> {
    let enum_type = *c
        .types
        .structs
        .get(enum_name)
        .ok_or_else(|| format!("unknown enum: {enum_name}"))?;
    let tag = c
        .types
        .get_variant_tag(enum_name, variant)
        .ok_or_else(|| format!("unknown variant: {enum_name}.{variant}"))?;
    let tag_ptr = c
        .builder
        .build_struct_gep(enum_type, subject_ptr, 0, "tag_ptr")
        .unwrap();
    let tag_val = c
        .builder
        .build_load(c.context.i8_type(), tag_ptr, "tag")
        .unwrap()
        .into_int_value();
    let expected = c.context.i8_type().const_int(tag as u64, false);
    Ok(c.builder
        .build_int_compare(IntPredicate::EQ, tag_val, expected, "tag_eq")
        .unwrap())
}

fn compile_tuple_elements<'ctx>(
    c: &mut Compiler<'ctx>,
    elements: &[Pattern],
    field_types: &[Type],
    payload_type: inkwell::types::StructType<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    mut result: IntValue<'ctx>,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    for (i, sub_pat) in elements.iter().enumerate() {
        let field_type = &field_types[i];
        let inner_ty = unwrap_indirect(field_type);
        // Align with monomorphized enum payloads: ZST fields use an i8 placeholder when
        // `to_llvm_type` is `None` (e.g. `()`), so LLVM layout and pattern loads stay in sync.
        let inner_llvm_ty = to_llvm_type(inner_ty, c.context, &c.types.structs)
            .unwrap_or_else(|| c.context.i8_type().into());
        let field_ptr = c
            .builder
            .build_struct_gep(payload_type, payload_ptr, i as u32, &format!("tp{i}"))
            .unwrap();
        let field_val = load_maybe_indirect(c, field_ptr, field_type, &format!("tp{i}_val"));
        let field_alloca = c
            .builder
            .build_alloca(inner_llvm_ty, &format!("tp{i}_tmp"))
            .unwrap();
        c.builder.build_store(field_alloca, field_val).unwrap();

        let sub_result = compile_pattern(c, sub_pat, field_alloca, inner_ty, function)?;
        result = c
            .builder
            .build_and(result, sub_result, &format!("tp{i}_and"))
            .unwrap();
    }
    Ok(result)
}

fn enum_name_from_path<'ctx>(
    c: &Compiler<'ctx>,
    type_path: &[String],
    subject_type: &Type,
) -> Result<String, String> {
    let ty = unwrap_indirect(subject_type);
    match ty {
        Type::GenericInstance {
            base,
            type_args,
            kind: GenericKind::Enum,
            ..
        } => Ok(mangle_name(base, type_args)),
        Type::Enum(name) => Ok(name.clone()),
        Type::Union(_) => Ok(mangle_type(ty)),
        Type::Struct(name) => {
            if let Some((base, _)) = crate::generics::try_parse_mangled_name(name, c)
                && c.type_ctx.is_enum(&base)
            {
                Ok(name.clone())
            } else if !type_path.is_empty() {
                let joined = type_path.join(".");
                if c.types.structs.contains_key(&joined)
                    || c.types.mono_enum_variants.contains_key(&joined)
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
            if c.types.structs.contains_key(&joined)
                || c.types.mono_enum_variants.contains_key(&joined)
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
    c: &Compiler<'ctx>,
    variant_name: &str,
    subject_type: &Type,
) -> Result<String, String> {
    let subject_type = unwrap_indirect(subject_type);
    if let Type::GenericInstance {
        base,
        type_args,
        kind: GenericKind::Enum,
        ..
    } = subject_type
    {
        return Ok(mangle_name(base, type_args));
    }
    if let Type::Enum(name) = subject_type {
        return Ok(name.clone());
    }
    if let Type::Struct(name) = subject_type {
        if let Some((base, _)) = crate::generics::try_parse_mangled_name(name, c)
            && c.type_ctx.is_enum(&base)
        {
            return Ok(name.clone());
        }
        if c.type_ctx.is_enum(name) {
            return Ok(name.clone());
        }
    }
    if let Type::Union(members) = subject_type {
        let member_mangled = mangle_type(&Type::Struct(variant_name.to_string()));
        if members.iter().any(|m| mangle_type(m) == member_mangled) {
            return Ok(mangle_type(subject_type));
        }
    }
    for (enum_name, info) in c.type_ctx.types.iter().filter(|(_, ti)| ti.is_enum()) {
        if info
            .variants()
            .is_some_and(|vs| vs.iter().any(|v| v.name == variant_name))
        {
            return Ok(enum_name.clone());
        }
    }
    Err(format!("no enum found with variant `{variant_name}`"))
}

fn get_payload_ptr<'ctx>(
    c: &mut Compiler<'ctx>,
    subject_ptr: PointerValue<'ctx>,
    enum_name: &str,
    variant: &str,
) -> Result<(inkwell::types::StructType<'ctx>, PointerValue<'ctx>), String> {
    let payload_type = c
        .types
        .get_variant_payload_type(enum_name, variant)
        .ok_or_else(|| format!("no payload type for {enum_name}.{variant}"))?;
    let enum_type = *c
        .types
        .structs
        .get(enum_name)
        .ok_or_else(|| format!("unknown enum: {enum_name}"))?;
    let payload_ptr = c
        .builder
        .build_struct_gep(enum_type, subject_ptr, 1, "payload_ptr")
        .unwrap();
    Ok((payload_type, payload_ptr))
}

fn get_struct_variant_fields(
    c: &Compiler<'_>,
    enum_name: &str,
    variant: &str,
) -> Result<Vec<(String, Type)>, String> {
    let data = lookup_variant_data(c, enum_name, variant)?;
    match data {
        VariantData::Struct(fields) => Ok(fields),
        _ => Err(format!("{enum_name}.{variant} is not a struct variant")),
    }
}

fn get_tuple_variant_types(
    c: &Compiler<'_>,
    enum_name: &str,
    variant: &str,
) -> Result<Vec<Type>, String> {
    let data = lookup_variant_data(c, enum_name, variant)?;
    match data {
        VariantData::Tuple(types) => Ok(types),
        _ => Err(format!("{enum_name}.{variant} is not a tuple variant")),
    }
}

fn lookup_variant_data(
    c: &Compiler<'_>,
    enum_name: &str,
    variant: &str,
) -> Result<VariantData, String> {
    if let Some(ti) = c.type_ctx.types.get(enum_name)
        && let Some(vs) = ti.variants()
        && let Some(vi) = vs.iter().find(|v| v.name == variant)
    {
        return Ok(vi.data.clone());
    }
    if let Some(variants) = c.types.mono_enum_variants.get(enum_name)
        && let Some((_, data)) = variants.iter().find(|(n, _)| n == variant)
    {
        return Ok(data.clone());
    }
    Err(format!("variant not found: {enum_name}.{variant}"))
}

fn match_values<'ctx>(
    c: &Compiler<'ctx>,
    subject: &BasicValueEnum<'ctx>,
    lit: &BasicValueEnum<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    if subject.is_int_value() && lit.is_int_value() {
        let subj_iv = subject.into_int_value();
        let mut lit_iv = lit.into_int_value();
        let subj_bits = subj_iv.get_type().get_bit_width();
        let lit_bits = lit_iv.get_type().get_bit_width();
        if subj_bits != lit_bits {
            let target_ty = c.context.custom_width_int_type(subj_bits);
            lit_iv = if subj_bits < lit_bits {
                c.builder
                    .build_int_truncate(lit_iv, target_ty, "lit_trunc")
                    .unwrap()
            } else {
                c.builder
                    .build_int_s_extend(lit_iv, target_ty, "lit_sext")
                    .unwrap()
            };
        }
        Ok(c.builder
            .build_int_compare(IntPredicate::EQ, subj_iv, lit_iv, "lit_eq")
            .unwrap())
    } else if subject.is_float_value() && lit.is_float_value() {
        Ok(c.builder
            .build_float_compare(
                FloatPredicate::OEQ,
                subject.into_float_value(),
                lit.into_float_value(),
                "lit_feq",
            )
            .unwrap())
    } else if subject.is_pointer_value() && lit.is_pointer_value() {
        let strcmp = *c.functions.get("strcmp").ok_or("strcmp not declared")?;
        let cmp_result = c
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
        let zero = c.context.i32_type().const_int(0, false);
        Ok(c.builder
            .build_int_compare(IntPredicate::EQ, cmp_result, zero, "str_eq")
            .unwrap())
    } else {
        Err("unsupported literal pattern comparison".to_string())
    }
}
