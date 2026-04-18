//! Enum codegen: variant construction and structural equality.

use std::collections::HashMap;

use expo_ast::ast::EnumConstructionData;
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{
    Type, TypeIdentifier, mangle_name, named_generic, unify, unwrap_indirect,
};
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue};

use expo_ir::resolved::enums::{ResolvedEnumEq, ResolvedVariantEq, ResolvedVariantFields};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::control::{get_payload_ptr, lookup_variant_data, match_values};
use crate::expr::{compile_expr, compile_expr_coerced};
use crate::generics::monomorphize_enum;
use crate::structs::{load_maybe_indirect, store_maybe_indirect};
use crate::types::to_llvm_type;

/// Compiles an enum variant construction (`EnumName.Variant(...)` or
/// `EnumName.Variant { ... }`). Sets the tag byte and populates the payload
/// for tuple and struct variants. For generic enums, infers type arguments,
/// triggers monomorphization, and uses the mangled name.
pub fn compile_enum_construction<'ctx>(
    compiler: &mut Compiler<'ctx>,
    type_path: &[String],
    variant: &str,
    data: &EnumConstructionData,
    resolved_type: Option<&TypeIdentifier>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let base_name = type_path
        .first()
        .ok_or("empty type path in enum construction")?;

    let resolved_id = compiler.id_for(base_name, resolved_type);
    let type_info = resolved_id
        .as_ref()
        .and_then(|id| compiler.type_ctx.get_type(id));

    let is_generic = type_info.is_some_and(|ti| ti.is_enum() && !ti.type_params.is_empty());

    if is_generic {
        return compile_generic_enum_construction(
            compiler,
            base_name,
            variant,
            data,
            resolved_id.as_ref(),
            function,
        );
    }

    compile_concrete_enum(
        compiler,
        base_name,
        variant,
        data,
        resolved_id.as_ref(),
        function,
    )
}

struct ResolvedEnumVariant<'ctx> {
    enum_type: StructType<'ctx>,
    payload_type: Option<StructType<'ctx>>,
    result_type: Type,
    tag: u64,
    variant_fields: ResolvedVariantFields,
}

fn resolve_concrete_enum_variant<'ctx>(
    compiler: &Compiler<'ctx>,
    enum_name: &str,
    variant: &str,
    data: &EnumConstructionData,
    resolved_type: Option<&TypeIdentifier>,
) -> Result<ResolvedEnumVariant<'ctx>, String> {
    // Prefer the typecheck-resolved identifier (which already honours aliases
    // and cross-package qualification); fall back to a package-aware lookup
    // from the current compilation scope when it's missing.
    let resolved_id = compiler
        .id_for(enum_name, resolved_type)
        .ok_or_else(|| format!("unknown enum type: {enum_name}"))?;

    let enum_type = compiler
        .types
        .get_concrete(&resolved_id)
        .ok_or_else(|| format!("unknown enum type: {resolved_id}"))?;

    let key = resolved_id.qualified_name();
    let tag = compiler
        .types
        .get_variant_tag(&key, variant)
        .ok_or_else(|| format!("unknown variant `{variant}` on enum `{resolved_id}`"))?
        as u64;

    let (payload_type, variant_fields) = match data {
        EnumConstructionData::Unit => (None, ResolvedVariantFields::Unit),
        EnumConstructionData::Tuple(_) => {
            let payload = compiler
                .types
                .get_variant_payload_type(&key, variant)
                .ok_or_else(|| format!("no payload type for {resolved_id}.{variant}"))?;

            let element_types = compiler
                .type_ctx
                .get_type(&resolved_id)
                .and_then(|ti| ti.variants())
                .and_then(|vs| vs.iter().find(|v| v.name == variant))
                .and_then(|vi| match &vi.data {
                    VariantData::Tuple(types) => Some(types.clone()),
                    _ => None,
                })
                .unwrap_or_default();

            (
                Some(payload),
                ResolvedVariantFields::Tuple { element_types },
            )
        }
        EnumConstructionData::Struct(field_inits) => {
            let payload = compiler
                .types
                .get_variant_payload_type(&key, variant)
                .ok_or_else(|| format!("no payload type for {resolved_id}.{variant}"))?;

            let variant_info = compiler
                .type_ctx
                .get_type(&resolved_id)
                .and_then(|ti| ti.variants())
                .and_then(|vs| vs.iter().find(|v| v.name == variant))
                .ok_or_else(|| format!("variant info not found for {resolved_id}.{variant}"))?;

            let expected_fields = match &variant_info.data {
                VariantData::Struct(f) => f,
                _ => {
                    return Err(format!("{resolved_id}.{variant} is not a struct variant"));
                }
            };

            let mut fields = Vec::new();
            for field_init in field_inits {
                let (idx, field_type) = expected_fields
                    .iter()
                    .enumerate()
                    .find(|(_, (name, _))| *name == field_init.name)
                    .map(|(i, (_, ty))| (i as u32, ty.clone()))
                    .ok_or_else(|| {
                        format!(
                            "unknown field `{}` in {resolved_id}.{variant}",
                            field_init.name,
                        )
                    })?;
                fields.push((field_init.name.clone(), idx, field_type));
            }

            (Some(payload), ResolvedVariantFields::Struct { fields })
        }
    };

    let result_type = Type::Named {
        identifier: resolved_id,
        type_args: vec![],
    };

    Ok(ResolvedEnumVariant {
        enum_type,
        payload_type,
        result_type,
        tag,
        variant_fields,
    })
}

fn compile_concrete_enum<'ctx>(
    compiler: &mut Compiler<'ctx>,
    enum_name: &str,
    variant: &str,
    data: &EnumConstructionData,
    resolved_type: Option<&TypeIdentifier>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let resolved =
        resolve_concrete_enum_variant(compiler, enum_name, variant, data, resolved_type)?;

    let alloca = compiler
        .builder
        .build_alloca(resolved.enum_type, &format!("{enum_name}_{variant}"))
        .unwrap();

    let tag_ptr = compiler
        .builder
        .build_struct_gep(resolved.enum_type, alloca, 0, "tag_ptr")
        .unwrap();
    let tag_val = compiler.context.i8_type().const_int(resolved.tag, false);
    compiler.builder.build_store(tag_ptr, tag_val).unwrap();

    match (&resolved.variant_fields, data) {
        (ResolvedVariantFields::Unit, _) => {}
        (ResolvedVariantFields::Tuple { element_types }, EnumConstructionData::Tuple(exprs)) => {
            let payload_type = resolved.payload_type.unwrap();
            let payload_ptr = compiler
                .builder
                .build_struct_gep(resolved.enum_type, alloca, 1, "payload_ptr")
                .unwrap();

            for (i, expr) in exprs.iter().enumerate() {
                let elem_type = element_types.get(i);
                let coerce_ty = elem_type.map(unwrap_indirect);
                let val = if let Some(ct) = coerce_ty {
                    compile_expr_coerced(compiler, expr, ct, function)?
                } else {
                    compile_expr(compiler, expr, function)?.map(|tv| tv.value)
                }
                .ok_or_else(|| format!("enum field {i} produced no value"))?;
                let field_ptr = compiler
                    .builder
                    .build_struct_gep(payload_type, payload_ptr, i as u32, &format!("field_{i}"))
                    .unwrap();
                if let Some(et) = elem_type {
                    store_maybe_indirect(
                        compiler,
                        field_ptr,
                        val,
                        et,
                        &format!("{enum_name}_{variant}_{i}"),
                    );
                } else {
                    compiler.builder.build_store(field_ptr, val).unwrap();
                }
            }
        }
        (ResolvedVariantFields::Struct { fields }, EnumConstructionData::Struct(field_inits)) => {
            let payload_type = resolved.payload_type.unwrap();
            let payload_ptr = compiler
                .builder
                .build_struct_gep(resolved.enum_type, alloca, 1, "payload_ptr")
                .unwrap();

            for (field_init, (_, field_idx, field_type)) in field_inits.iter().zip(fields.iter()) {
                let coerce_ty = unwrap_indirect(field_type);
                let val =
                    compile_expr_coerced(compiler, &field_init.value, coerce_ty, function)?
                        .ok_or_else(|| format!("field `{}` produced no value", field_init.name))?;
                let field_ptr = compiler
                    .builder
                    .build_struct_gep(payload_type, payload_ptr, *field_idx, &field_init.name)
                    .unwrap();
                store_maybe_indirect(compiler, field_ptr, val, field_type, &field_init.name);
            }
        }
        _ => {}
    }

    let enum_val = compiler
        .builder
        .build_load(resolved.enum_type, alloca, enum_name)
        .unwrap();
    Ok(Some(TypedValue::new(enum_val, resolved.result_type)))
}

struct ResolvedGenericEnum<'ctx> {
    enum_type: StructType<'ctx>,
    mangled_name: String,
    payload_type: Option<StructType<'ctx>>,
    result_type: Type,
    tag: u64,
    variant_element_types: Option<Vec<Type>>,
}

fn resolve_generic_enum<'ctx>(
    compiler: &mut Compiler<'ctx>,
    enum_name: &str,
    variant: &str,
    resolved_type: Option<&TypeIdentifier>,
    data: &EnumConstructionData,
    compiled_values: &[BasicValueEnum<'ctx>],
    compiled_types: &[Type],
) -> Result<ResolvedGenericEnum<'ctx>, String> {
    let resolved_id = compiler.id_for(enum_name, resolved_type);
    let enum_info = resolved_id
        .as_ref()
        .and_then(|id| compiler.type_ctx.get_type(id))
        .filter(|ti| ti.is_enum())
        .cloned()
        .ok_or_else(|| format!("no enum info for `{enum_name}`"))?;

    let vi = enum_info
        .variants()
        .and_then(|vs| vs.iter().find(|v| v.name == variant))
        .ok_or_else(|| format!("unknown variant `{variant}` on enum `{enum_name}`"))?;

    let mut subst: HashMap<String, Type> = HashMap::new();
    match (data, &vi.data) {
        (EnumConstructionData::Tuple(_), VariantData::Tuple(expected)) => {
            for (i, compiled_type) in compiled_types.iter().enumerate() {
                if i < expected.len() {
                    unify(&expected[i], compiled_type, &mut subst);
                }
            }
        }
        (EnumConstructionData::Unit, _) => {}
        _ => {
            return Err(format!(
                "unsupported generic enum construction for {enum_name}.{variant}"
            ));
        }
    }

    let mut type_args: Vec<Type> = enum_info
        .type_params
        .iter()
        .map(|tp| {
            subst
                .get(&tp.name)
                .cloned()
                .or_else(|| compiler.fn_state.type_subst.get(&tp.name).cloned())
                .unwrap_or(Type::Unknown)
        })
        .collect();

    let has_unknown = type_args.contains(&Type::Unknown);
    if has_unknown && let Some(ref hint) = compiler.fn_state.return_type_hint {
        let hint_args = match hint {
            Type::Named {
                identifier,
                type_args: ha,
            } if identifier.name == enum_name && !ha.is_empty() => Some(ha.clone()),
            Type::Named { identifier, .. } => {
                crate::generics::try_parse_mangled_name(&identifier.name, compiler)
                    .filter(|(base, _)| base == enum_name)
                    .map(|(_, ha)| ha)
            }
            _ => None,
        };
        if let Some(ha) = hint_args {
            for (i, ta) in type_args.iter_mut().enumerate() {
                if *ta == Type::Unknown && i < ha.len() {
                    *ta = ha[i].clone();
                }
            }
        }
    }

    // We need a resolved TypeIdentifier here for the mangled key; without a
    // package we cannot guarantee uniqueness across crates.
    let enum_id = resolved_id.ok_or_else(|| {
        format!("cannot resolve package for generic enum `{enum_name}` during construction")
    })?;
    let mangled_name = mangle_name(&enum_id, &type_args);

    if !compiler.types.contains_monomorphized(&mangled_name) {
        monomorphize_enum(compiler, &enum_id, &type_args)?;
    }

    let enum_type = compiler
        .types
        .get_monomorphized(&mangled_name)
        .ok_or_else(|| format!("monomorphized enum `{mangled_name}` not found"))?;

    let tag = compiler
        .types
        .get_variant_tag(&mangled_name, variant)
        .ok_or_else(|| format!("unknown variant `{variant}` on enum `{mangled_name}`"))?
        as u64;

    let payload_type = if !compiled_values.is_empty() {
        Some(
            compiler
                .types
                .get_variant_payload_type(&mangled_name, variant)
                .ok_or_else(|| format!("no payload type for {mangled_name}.{variant}"))?,
        )
    } else {
        None
    };

    let variant_element_types: Option<Vec<Type>> = compiler
        .types
        .mono_enum_variants
        .get(&mangled_name)
        .and_then(|vs| vs.iter().find(|(n, _)| n == variant))
        .and_then(|(_, vdata)| match vdata {
            VariantData::Tuple(types) => Some(types.clone()),
            _ => None,
        });

    let result_type = named_generic(
        enum_name,
        type_args,
        compiler.type_ctx,
        compiler.current_package.as_ref(),
    );

    Ok(ResolvedGenericEnum {
        enum_type,
        mangled_name,
        payload_type,
        result_type,
        tag,
        variant_element_types,
    })
}

fn compile_generic_enum_construction<'ctx>(
    compiler: &mut Compiler<'ctx>,
    enum_name: &str,
    variant: &str,
    data: &EnumConstructionData,
    resolved_type: Option<&TypeIdentifier>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let mut compiled_values: Vec<BasicValueEnum<'ctx>> = Vec::new();
    let mut compiled_types: Vec<Type> = Vec::new();

    if let EnumConstructionData::Tuple(exprs) = data {
        for (i, expr) in exprs.iter().enumerate() {
            let tv = compile_expr(compiler, expr, function)?
                .ok_or_else(|| format!("enum field {i} produced no value"))?;
            compiled_types.push(tv.expo_type);
            compiled_values.push(tv.value);
        }
    }

    let resolved = resolve_generic_enum(
        compiler,
        enum_name,
        variant,
        resolved_type,
        data,
        &compiled_values,
        &compiled_types,
    )?;

    let alloca = compiler
        .builder
        .build_alloca(
            resolved.enum_type,
            &format!("{}_{variant}", resolved.mangled_name),
        )
        .unwrap();

    let tag_ptr = compiler
        .builder
        .build_struct_gep(resolved.enum_type, alloca, 0, "tag_ptr")
        .unwrap();
    let tag_val = compiler.context.i8_type().const_int(resolved.tag, false);
    compiler.builder.build_store(tag_ptr, tag_val).unwrap();

    if !compiled_values.is_empty() {
        let payload_type = resolved.payload_type.unwrap();
        let payload_ptr = compiler
            .builder
            .build_struct_gep(resolved.enum_type, alloca, 1, "payload_ptr")
            .unwrap();

        for (i, val) in compiled_values.iter().enumerate() {
            let field_ptr = compiler
                .builder
                .build_struct_gep(payload_type, payload_ptr, i as u32, &format!("field_{i}"))
                .unwrap();
            if let Some(ref types) = resolved.variant_element_types
                && i < types.len()
            {
                store_maybe_indirect(
                    compiler,
                    field_ptr,
                    *val,
                    &types[i],
                    &format!("{}_{variant}_{i}", resolved.mangled_name),
                );
            } else {
                compiler.builder.build_store(field_ptr, *val).unwrap();
            }
        }
    }

    let enum_val = compiler
        .builder
        .build_load(resolved.enum_type, alloca, &resolved.mangled_name)
        .unwrap();
    Ok(Some(TypedValue::new(enum_val, resolved.result_type)))
}

// ---------------------------------------------------------------------------
// Enum equality
// ---------------------------------------------------------------------------

/// Resolves the mangled LLVM enum name from an Expo type.
pub(crate) fn enum_mangled_name(ty: &Type) -> Option<String> {
    match unwrap_indirect(ty) {
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => Some(mangle_name(identifier, type_args)),
        Type::Named { identifier, .. } => Some(identifier.qualified_name()),
        _ => None,
    }
}

fn compile_typed_value_eq<'ctx>(
    c: &mut Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    ty: &Type,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    if enum_mangled_name(ty).is_some() {
        return compile_enum_struct_eq(c, lhs, rhs, ty, function);
    }
    match_values(c, &lhs, &rhs)
}

fn resolve_enum_eq(c: &Compiler, ty: &Type) -> Result<ResolvedEnumEq, String> {
    let mangled = enum_mangled_name(ty)
        .ok_or_else(|| "compile_enum_struct_eq called with non-enum type".to_string())?;

    let payloads = c
        .types
        .enum_variant_payloads
        .get(&mangled)
        .ok_or_else(|| format!("enum variant payloads not found for `{mangled}`"))?;

    let mut variants = Vec::with_capacity(payloads.len());
    for (name, _) in payloads {
        let vdata = lookup_variant_data(c, &mangled, name)?;
        let resolved = match &vdata {
            VariantData::Struct(fields) => ResolvedVariantEq::Struct {
                field_types: fields.iter().map(|(_, t)| t.clone()).collect(),
            },
            VariantData::Tuple(types) => ResolvedVariantEq::Tuple {
                field_types: types.clone(),
            },
            VariantData::Unit => ResolvedVariantEq::Unit,
        };
        variants.push((name.clone(), resolved));
    }

    Ok(ResolvedEnumEq { mangled, variants })
}

/// Branch the current insert block into `merge_bb` and record `(value, predecessor)`
/// for a downstream phi.
///
/// Always uses `get_insert_block()` rather than the block we *think* we are in,
/// because nested calls (e.g. recursive enum-equality on a payload field) may
/// have left the builder positioned at an inner merge block. Trusting a stale
/// block here is exactly how "PHINode predecessors mismatch" verifier errors
/// sneak in.
fn branch_to_merge_phi<'ctx>(
    c: &Compiler<'ctx>,
    merge_bb: BasicBlock<'ctx>,
    value: BasicValueEnum<'ctx>,
    incoming: &mut Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)>,
) {
    let pred = c.builder.get_insert_block().unwrap();
    c.builder.build_unconditional_branch(merge_bb).unwrap();
    incoming.push((value, pred));
}

/// Structural `==` for two enum LLVM struct values (tag + optional payload).
pub(crate) fn compile_enum_struct_eq<'ctx>(
    c: &mut Compiler<'ctx>,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    ty: &Type,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let resolved = resolve_enum_eq(c, ty)?;

    let enum_type = to_llvm_type(ty, c.context, &c.types)
        .map(|t| t.into_struct_type())
        .ok_or_else(|| format!("unknown enum LLVM type: {}", resolved.mangled))?;

    let lhs_ptr = c.builder.build_alloca(enum_type, "enum_eq_l").unwrap();
    let rhs_ptr = c.builder.build_alloca(enum_type, "enum_eq_r").unwrap();
    c.builder
        .build_store(lhs_ptr, lhs.into_struct_value())
        .unwrap();
    c.builder
        .build_store(rhs_ptr, rhs.into_struct_value())
        .unwrap();

    let i8_ty = c.context.i8_type();
    let tag_l = c
        .builder
        .build_load(
            i8_ty,
            c.builder
                .build_struct_gep(enum_type, lhs_ptr, 0, "tag_l_ptr")
                .unwrap(),
            "tag_l",
        )
        .unwrap()
        .into_int_value();
    let tag_r = c
        .builder
        .build_load(
            i8_ty,
            c.builder
                .build_struct_gep(enum_type, rhs_ptr, 0, "tag_r_ptr")
                .unwrap(),
            "tag_r",
        )
        .unwrap()
        .into_int_value();

    let parent_fn = c.builder.get_insert_block().unwrap().get_parent().unwrap();
    let bb_tags_diff = c.context.append_basic_block(parent_fn, "enum_eq_tags_diff");
    let bb_tags_same = c.context.append_basic_block(parent_fn, "enum_eq_tags_same");
    let merge_bb = c.context.append_basic_block(parent_fn, "enum_eq_merge");

    let tags_match = c
        .builder
        .build_int_compare(IntPredicate::EQ, tag_l, tag_r, "tags_match")
        .unwrap();
    c.builder
        .build_conditional_branch(tags_match, bb_tags_same, bb_tags_diff)
        .unwrap();

    c.builder.position_at_end(bb_tags_diff);
    let false_val = c.context.bool_type().const_int(0, false);
    let mut incoming: Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> = Vec::new();
    branch_to_merge_phi(c, merge_bb, false_val.into(), &mut incoming);

    c.builder.position_at_end(bb_tags_same);
    let i1_ty = c.context.bool_type();

    let mut variant_bbs = Vec::with_capacity(resolved.variants.len());
    let mut switch_cases = Vec::with_capacity(resolved.variants.len());
    for i in 0..resolved.variants.len() {
        let bb = c
            .context
            .append_basic_block(parent_fn, &format!("enum_eq_v{i}"));
        variant_bbs.push(bb);
        switch_cases.push((i8_ty.const_int(i as u64, false), bb));
    }

    let bb_default = c.context.append_basic_block(parent_fn, "enum_eq_bad_tag");
    c.builder
        .build_switch(tag_l, bb_default, &switch_cases)
        .unwrap();

    for (i, (vname, variant_eq)) in resolved.variants.iter().enumerate() {
        c.builder.position_at_end(variant_bbs[i]);

        let field_types = match variant_eq {
            ResolvedVariantEq::Struct { field_types }
            | ResolvedVariantEq::Tuple { field_types } => Some(field_types),
            ResolvedVariantEq::Unit => None,
        };

        let eq_val = match field_types {
            None => i1_ty.const_int(1, false),
            Some(fields) => {
                let (payload_type, lp) = get_payload_ptr(c, lhs_ptr, &resolved.mangled, vname)?;
                let (_pt, rp) = get_payload_ptr(c, rhs_ptr, &resolved.mangled, vname)?;

                let mut acc: Option<IntValue<'ctx>> = None;
                for (fi, fty) in fields.iter().enumerate() {
                    let lf = c
                        .builder
                        .build_struct_gep(payload_type, lp, fi as u32, &format!("eq_lf{fi}"))
                        .unwrap();
                    let rf = c
                        .builder
                        .build_struct_gep(payload_type, rp, fi as u32, &format!("eq_rf{fi}"))
                        .unwrap();
                    let lv = load_maybe_indirect(c, lf, fty, &format!("eq_lv{fi}"));
                    let rv = load_maybe_indirect(c, rf, fty, &format!("eq_rv{fi}"));
                    let cmp = compile_typed_value_eq(c, lv, rv, fty, function)?;
                    acc = Some(match acc {
                        None => cmp,
                        Some(prev) => c
                            .builder
                            .build_and(prev, cmp, &format!("eq_and{fi}"))
                            .unwrap(),
                    });
                }
                acc.unwrap_or_else(|| i1_ty.const_int(1, false))
            }
        };

        branch_to_merge_phi(c, merge_bb, eq_val.into(), &mut incoming);
    }

    c.builder.position_at_end(bb_default);
    branch_to_merge_phi(c, merge_bb, false_val.into(), &mut incoming);

    c.builder.position_at_end(merge_bb);
    let phi = c.builder.build_phi(i1_ty, "enum_eq_phi").unwrap();
    for (v, bb) in &incoming {
        phi.add_incoming(&[(v, *bb)]);
    }

    Ok(phi.as_basic_value().into_int_value())
}
