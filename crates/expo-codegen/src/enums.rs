//! Enum codegen: variant construction and structural equality.
//!
//! Both halves follow the lower/emit split established by
//! `control/patterns.rs`.
//!
//! ## Construction
//!
//! - [`lower_enum_construction`] consumes the AST `EnumConstructionData`
//!   plus the type-checker's resolved identifier and produces a
//!   [`ResolvedEnumConstruction`]. All package-aware enum lookup, generic
//!   monomorphization, variant tag/payload-shape resolution, and
//!   `unify`-driven type-arg inference happens here. This is the only side
//!   that touches `compiler.layouts`, `compiler.llvm_types`,
//!   `compiler.type_ctx`, or `monomorphize_*`.
//!
//! - [`emit_enum_construction`] consumes the resolved IR plus the AST data
//!   and emits LLVM IR (alloca, store-tag, GEP-into-payload, store-fields,
//!   load-result). It only performs deterministic `Type` -> `BasicTypeEnum`
//!   translations, builder calls, and the [`store_maybe_indirect`] helper.
//!
//! [`compile_enum_construction`] is the public entry point and a thin
//! shim over those two phases. For generics, the shim pre-compiles the
//! tuple arguments so lower can drive `unify` over their resolved types --
//! see the design note in `expo/design/archive/20260502-EXPOIR-ROADMAP.md`
//! (superseded by `expo/design/COMPILER-NORTHSTAR.md`) for why the boundary
//! relaxes here vs. patterns.
//!
//! ## Equality
//!
//! [`compile_enum_struct_eq`] is already split: [`resolve_enum_eq`]
//! produces a [`ResolvedEnumEq`] and the emission code below walks it.

use std::collections::HashMap;

use expo_ast::ast::{EnumConstructionData, Expr, FieldInit};
use expo_ir::identity::{MonomorphizedTypeIdentifier, VariantIdentifier};
use expo_ir::lower::enums::{
    enum_mangled_name, lower_concrete_enum, resolve_enum_eq, resolve_generic_type_args,
};
use expo_ir::lower::types::id_for;
use expo_ir::resolved::construction::ResolvedEnumConstruction;
use expo_ir::resolved::enums::{ResolvedVariantEq, ResolvedVariantFields};
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{
    Type, TypeIdentifier, mangle_name, named_generic, unify, unwrap_indirect,
};
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::StructType;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::control::{get_payload_ptr, match_values};
use crate::expr::{compile_expr, compile_expr_coerced};
use crate::generics::monomorphize_enum;
use crate::structs::{load_maybe_indirect, store_maybe_indirect};
use crate::types::to_llvm_type;

/// Compiles an enum variant construction (`EnumName.Variant(...)` or
/// `EnumName.Variant { ... }`). Thin lower/emit shim. For generic enums,
/// pre-compiles the tuple arguments so [`lower_enum_construction`] can
/// drive `unify` over their resolved types before triggering monomorphization.
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

    let is_generic = id_for(&compiler.lower_ctx(), base_name, resolved_type)
        .as_ref()
        .and_then(|id| compiler.type_ctx.get_type(id))
        .is_some_and(|ti| ti.is_enum() && !ti.type_params.is_empty());

    let pre_compiled = if is_generic {
        precompile_generic_tuple_args(compiler, data, function)?
    } else {
        PreCompiledArgs::default()
    };

    let resolved = lower_enum_construction(
        compiler,
        base_name,
        variant,
        data,
        resolved_type,
        &pre_compiled.types,
    )?;

    emit_enum_construction(compiler, &resolved, data, &pre_compiled.values, function)
}

/// Pre-compiled tuple arguments for the generic enum-construction path,
/// where lower needs the resolved types to drive `unify`.
#[derive(Default)]
struct PreCompiledArgs<'ctx> {
    types: Vec<Type>,
    values: Vec<BasicValueEnum<'ctx>>,
}

fn precompile_generic_tuple_args<'ctx>(
    compiler: &mut Compiler<'ctx>,
    data: &EnumConstructionData,
    function: FunctionValue<'ctx>,
) -> Result<PreCompiledArgs<'ctx>, String> {
    let EnumConstructionData::Tuple(exprs) = data else {
        return Ok(PreCompiledArgs::default());
    };

    let mut types = Vec::with_capacity(exprs.len());
    let mut values = Vec::with_capacity(exprs.len());
    for (i, expr) in exprs.iter().enumerate() {
        let tv = compile_expr(compiler, expr, function)?
            .ok_or_else(|| format!("enum field {i} produced no value"))?;
        types.push(tv.expo_type);
        values.push(tv.value);
    }
    Ok(PreCompiledArgs { types, values })
}

// ---------------------------------------------------------------------------
// Lowering
// ---------------------------------------------------------------------------

/// Lowers an enum construction to its resolved IR. Handles both concrete and
/// generic enums uniformly: for generics, runs `unify` over the supplied
/// `compiled_arg_types` and triggers monomorphization. The returned
/// `mangled_name` is always the post-monomorphization key suitable for
/// `compiler.llvm_types.get_monomorphized` / `get_concrete`.
fn lower_enum_construction(
    compiler: &mut Compiler,
    enum_name: &str,
    variant: &str,
    data: &EnumConstructionData,
    resolved_type: Option<&TypeIdentifier>,
    compiled_arg_types: &[Type],
) -> Result<ResolvedEnumConstruction, String> {
    let resolved_id = id_for(&compiler.lower_ctx(), enum_name, resolved_type);
    let info = resolved_id
        .as_ref()
        .and_then(|id| compiler.type_ctx.get_type(id))
        .filter(|ti| ti.is_enum());

    let is_generic = info.is_some_and(|ti| !ti.type_params.is_empty());

    if is_generic {
        return lower_generic_enum(
            compiler,
            enum_name,
            variant,
            data,
            resolved_id,
            compiled_arg_types,
        );
    }

    lower_concrete_enum(&compiler.lower_ctx(), enum_name, variant, data, resolved_id)
}

fn lower_generic_enum(
    compiler: &mut Compiler,
    enum_name: &str,
    variant: &str,
    data: &EnumConstructionData,
    resolved_id: Option<TypeIdentifier>,
    compiled_arg_types: &[Type],
) -> Result<ResolvedEnumConstruction, String> {
    let enum_info = resolved_id
        .as_ref()
        .and_then(|id| compiler.type_ctx.get_type(id))
        .filter(|ti| ti.is_enum())
        .cloned()
        .ok_or_else(|| format!("no enum info for `{enum_name}`"))?;

    let variant_info = enum_info
        .variants()
        .and_then(|vs| vs.iter().find(|v| v.name == variant))
        .ok_or_else(|| format!("unknown variant `{variant}` on enum `{enum_name}`"))?;

    let subst = unify_generic_enum_args(data, &variant_info.data, compiled_arg_types, enum_name)?;
    let type_args = resolve_generic_type_args(
        &compiler.lower_ctx(),
        &enum_info.type_params,
        &subst,
        enum_name,
    );

    let enum_id = resolved_id.ok_or_else(|| {
        format!("cannot resolve package for generic enum `{enum_name}` during construction")
    })?;
    let mangled_name = mangle_name(&enum_id, &type_args);

    if !compiler
        .llvm_types
        .contains_monomorphized(&MonomorphizedTypeIdentifier::new(&mangled_name))
    {
        monomorphize_enum(compiler, &enum_id, &type_args)?;
    }

    let tag = compiler
        .layouts
        .variant_index(&MonomorphizedTypeIdentifier::new(&mangled_name), variant)
        .ok_or_else(|| format!("unknown variant `{variant}` on enum `{mangled_name}`"))?
        as u64;

    let element_types = compiler
        .layouts
        .enum_variants(&MonomorphizedTypeIdentifier::new(&mangled_name))
        .and_then(|vs| vs.iter().find(|(n, _)| n == variant))
        .and_then(|(_, vdata)| match vdata {
            VariantData::Tuple(types) => Some(types.clone()),
            _ => None,
        })
        .unwrap_or_default();

    let variant_fields = match data {
        EnumConstructionData::Unit => ResolvedVariantFields::Unit,
        EnumConstructionData::Tuple(_) => ResolvedVariantFields::Tuple { element_types },
        EnumConstructionData::Struct(_) => {
            return Err(format!(
                "unsupported generic enum construction for {enum_name}.{variant}"
            ));
        }
    };

    let result_type = named_generic(
        enum_name,
        type_args,
        compiler.type_ctx,
        compiler.current_package.as_ref(),
    );

    Ok(ResolvedEnumConstruction {
        is_generic: true,
        mangled_name: MonomorphizedTypeIdentifier::new(&mangled_name),
        result_type,
        tag,
        variant_fields,
        variant_name: variant.to_string(),
    })
}

fn unify_generic_enum_args(
    data: &EnumConstructionData,
    variant_data: &VariantData,
    compiled_arg_types: &[Type],
    enum_name: &str,
) -> Result<HashMap<String, Type>, String> {
    let mut subst: HashMap<String, Type> = HashMap::new();
    match (data, variant_data) {
        (EnumConstructionData::Tuple(_), VariantData::Tuple(expected)) => {
            for (i, compiled_type) in compiled_arg_types.iter().enumerate() {
                if i < expected.len() {
                    unify(&expected[i], compiled_type, &mut subst);
                }
            }
        }
        (EnumConstructionData::Unit, _) => {}
        _ => {
            return Err(format!(
                "unsupported generic enum construction for {enum_name}"
            ));
        }
    }
    Ok(subst)
}

// ---------------------------------------------------------------------------
// Emission
// ---------------------------------------------------------------------------

/// Emits LLVM IR for a lowered enum construction. Allocates the enum, stores
/// the tag, and writes payload fields. For the generic path, callers supply
/// `pre_compiled_values` (already evaluated to drive `unify`); for concrete,
/// the slice is empty and emit walks `data` itself with per-field coercion.
fn emit_enum_construction<'ctx>(
    compiler: &mut Compiler<'ctx>,
    resolved: &ResolvedEnumConstruction,
    data: &EnumConstructionData,
    pre_compiled_values: &[BasicValueEnum<'ctx>],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let enum_type = lookup_enum_llvm_type(compiler, resolved)?;
    let alloca_label = format!("{}_{}", resolved.mangled_name, resolved.variant_name);
    let alloca = compiler
        .builder
        .build_alloca(enum_type, &alloca_label)
        .unwrap();

    store_variant_tag(compiler, enum_type, alloca, resolved.tag);

    if !matches!(resolved.variant_fields, ResolvedVariantFields::Unit) {
        let payload_ptr = compiler
            .builder
            .build_struct_gep(enum_type, alloca, 1, "payload_ptr")
            .unwrap();
        emit_variant_payload(
            compiler,
            resolved,
            data,
            pre_compiled_values,
            payload_ptr,
            function,
        )?;
    }

    let enum_val = compiler
        .builder
        .build_load(enum_type, alloca, resolved.mangled_name.as_str())
        .unwrap();
    Ok(Some(TypedValue::new(
        enum_val,
        resolved.result_type.clone(),
    )))
}

fn lookup_enum_llvm_type<'ctx>(
    compiler: &Compiler<'ctx>,
    resolved: &ResolvedEnumConstruction,
) -> Result<StructType<'ctx>, String> {
    if resolved.is_generic {
        return compiler
            .llvm_types
            .get_monomorphized(&resolved.mangled_name)
            .ok_or_else(|| format!("monomorphized enum `{}` not found", resolved.mangled_name));
    }
    if let Type::Named { identifier, .. } = &resolved.result_type
        && let Some(t) = compiler.llvm_types.get_concrete(identifier)
    {
        return Ok(t);
    }
    Err(format!("unknown enum type: {}", resolved.mangled_name))
}

/// Write the variant tag (an `i8`) at slot 0 of an enum alloca.
/// Shared by both the legacy AST-driven [`emit_enum_construction`] in
/// this module and the IR-driven `emit_enum_construct` in
/// [`crate::control::instructions`].
pub(crate) fn store_variant_tag<'ctx>(
    compiler: &Compiler<'ctx>,
    enum_type: StructType<'ctx>,
    alloca: PointerValue<'ctx>,
    tag: u64,
) {
    let tag_ptr = compiler
        .builder
        .build_struct_gep(enum_type, alloca, 0, "tag_ptr")
        .unwrap();
    let tag_val = compiler.context.i8_type().const_int(tag, false);
    compiler.builder.build_store(tag_ptr, tag_val).unwrap();
}

fn emit_variant_payload<'ctx>(
    compiler: &mut Compiler<'ctx>,
    resolved: &ResolvedEnumConstruction,
    data: &EnumConstructionData,
    pre_compiled_values: &[BasicValueEnum<'ctx>],
    payload_ptr: PointerValue<'ctx>,
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    let id = VariantIdentifier::new(&resolved.mangled_name, &resolved.variant_name);
    let payload_type = compiler.llvm_types.variant_payload(&id).ok_or_else(|| {
        format!(
            "no payload type for {}.{}",
            resolved.mangled_name, resolved.variant_name
        )
    })?;

    match (&resolved.variant_fields, data) {
        (ResolvedVariantFields::Tuple { element_types }, EnumConstructionData::Tuple(exprs)) => {
            emit_tuple_payload(
                compiler,
                resolved,
                element_types,
                exprs,
                pre_compiled_values,
                payload_type,
                payload_ptr,
                function,
            )
        }
        (ResolvedVariantFields::Struct { fields }, EnumConstructionData::Struct(field_inits)) => {
            emit_struct_payload(
                compiler,
                fields,
                field_inits,
                payload_type,
                payload_ptr,
                function,
            )
        }
        (ResolvedVariantFields::Unit, _) => Ok(()),
        _ => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_tuple_payload<'ctx>(
    compiler: &mut Compiler<'ctx>,
    resolved: &ResolvedEnumConstruction,
    element_types: &[Type],
    exprs: &[Expr],
    pre_compiled_values: &[BasicValueEnum<'ctx>],
    payload_type: StructType<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    for (i, val) in materialize_tuple_values(
        compiler,
        exprs,
        pre_compiled_values,
        element_types,
        function,
    )?
    .into_iter()
    .enumerate()
    {
        let elem_type = element_types.get(i);
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
                &format!("{}_{}_{i}", resolved.mangled_name, resolved.variant_name),
            );
        } else {
            compiler.builder.build_store(field_ptr, val).unwrap();
        }
    }
    Ok(())
}

fn materialize_tuple_values<'ctx>(
    compiler: &mut Compiler<'ctx>,
    exprs: &[Expr],
    pre_compiled_values: &[BasicValueEnum<'ctx>],
    element_types: &[Type],
    function: FunctionValue<'ctx>,
) -> Result<Vec<BasicValueEnum<'ctx>>, String> {
    if !pre_compiled_values.is_empty() {
        return Ok(pre_compiled_values.to_vec());
    }

    let mut values = Vec::with_capacity(exprs.len());
    for (i, expr) in exprs.iter().enumerate() {
        let val = if let Some(et) = element_types.get(i) {
            compile_expr_coerced(compiler, expr, unwrap_indirect(et), function)?
                .ok_or_else(|| format!("enum field {i} produced no value"))?
        } else {
            compile_expr(compiler, expr, function)?
                .map(|tv| tv.value)
                .ok_or_else(|| format!("enum field {i} produced no value"))?
        };
        values.push(val);
    }
    Ok(values)
}

fn emit_struct_payload<'ctx>(
    compiler: &mut Compiler<'ctx>,
    fields: &[(String, u32, Type)],
    field_inits: &[FieldInit],
    payload_type: StructType<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    for (field_init, (_, field_idx, field_type)) in field_inits.iter().zip(fields.iter()) {
        let val = compile_expr_coerced(
            compiler,
            &field_init.value,
            unwrap_indirect(field_type),
            function,
        )?
        .ok_or_else(|| format!("field `{}` produced no value", field_init.name))?;
        let field_ptr = compiler
            .builder
            .build_struct_gep(payload_type, payload_ptr, *field_idx, &field_init.name)
            .unwrap();
        store_maybe_indirect(compiler, field_ptr, val, field_type, &field_init.name);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Enum equality
// ---------------------------------------------------------------------------

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
    let resolved = resolve_enum_eq(&c.lower_ctx(), ty)?;

    let enum_type = to_llvm_type(ty, c.context, &c.llvm_types)
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
                let (payload_type, lp) =
                    get_payload_ptr(c, lhs_ptr, resolved.mangled.as_str(), vname)?;
                let (_pt, rp) = get_payload_ptr(c, rhs_ptr, resolved.mangled.as_str(), vname)?;

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
