//! Debug protocol support: synthesizes `format` functions for enums and
//! structs, and provides `call_format` to invoke `{Type}_format` on any value.

use expo_typecheck::context::VariantData;
use expo_typecheck::types::{Primitive, Type};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};

use crate::compiler::Compiler;
use crate::hashtable::type_display_name;

/// Calls `{Type}_format(val)` and returns the resulting string pointer.
/// Synthesizes the format function on demand for enums and structs if it
/// doesn't already exist. Falls back to LLVM type inspection when the
/// Expo type is unknown.
pub fn call_format<'ctx>(
    c: &mut Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    expo_type: &Type,
) -> Result<PointerValue<'ctx>, String> {
    let resolved_type = if matches!(expo_type, Type::Unknown) {
        infer_type_from_llvm(val)
    } else {
        expo_type.clone()
    };

    let type_name = type_display_name(&resolved_type);
    let fn_name = format!("{type_name}_format");

    if !c.functions.contains_key(&fn_name) {
        if c.type_ctx.is_enum(&type_name) {
            synthesize_enum_format(c, &type_name)?;
        } else if c.type_ctx.is_struct(&type_name) {
            synthesize_struct_format(c, &type_name)?;
        } else {
            return Err(format!("no format function for type `{type_name}`"));
        }
    }

    let format_fn = *c
        .functions
        .get(&fn_name)
        .ok_or_else(|| format!("no format function for type `{type_name}`"))?;

    c.call(format_fn, &[val.into()], "fmt_result")
        .map(|v| v.into_pointer_value())
        .ok_or_else(|| format!("{fn_name} did not return a value"))
}

fn infer_type_from_llvm(val: BasicValueEnum) -> Type {
    if val.is_int_value() {
        let width = val.into_int_value().get_type().get_bit_width();
        match width {
            1 => Type::Primitive(Primitive::Bool),
            8 => Type::Primitive(Primitive::I8),
            16 => Type::Primitive(Primitive::I16),
            32 => Type::Primitive(Primitive::I32),
            _ => Type::Primitive(Primitive::I64),
        }
    } else if val.is_float_value() {
        let width = val.into_float_value().get_type();
        if width == width.get_context().f32_type() {
            Type::Primitive(Primitive::F32)
        } else {
            Type::Primitive(Primitive::F64)
        }
    } else if val.is_pointer_value() {
        Type::Primitive(Primitive::String)
    } else if val.is_struct_value() {
        let st = val.into_struct_value().get_type();
        if let Some(name) = st.get_name().and_then(|n| n.to_str().ok()) {
            Type::Struct(name.to_string())
        } else {
            Type::Unknown
        }
    } else {
        Type::Unknown
    }
}

/// Pre-synthesizes `{Type}_format` functions for all user-defined structs and
/// enums. Call after types are registered and functions are declared, but
/// before function bodies are compiled.
pub fn synthesize_all_formats<'ctx>(c: &mut Compiler<'ctx>) -> Result<(), String> {
    let type_names: Vec<String> = c
        .type_ctx
        .types
        .iter()
        .filter(|(_, ti)| (ti.is_struct() || ti.is_enum()) && ti.type_params.is_empty())
        .map(|(n, _)| n.clone())
        .collect();

    for name in &type_names {
        let fn_name = format!("{name}_format");
        if c.functions.contains_key(&fn_name) {
            continue;
        }
        if has_unsynthesizable_fields(c, name) {
            continue;
        }
        if c.type_ctx.is_enum(name) {
            synthesize_enum_format(c, name)?;
        } else if c.type_ctx.is_struct(name) {
            synthesize_struct_format(c, name)?;
        }
    }
    Ok(())
}

fn has_unsynthesizable_fields(c: &Compiler, name: &str) -> bool {
    fn is_complex(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Indirect(_) | Type::GenericInstance { .. } | Type::Unknown
        )
    }

    if let Some(ti) = c.type_ctx.types.get(name) {
        if let Some(fields) = ti.fields() {
            return fields.iter().any(|(_, ty)| is_complex(ty));
        }
        if let Some(variants) = ti.variants() {
            return variants.iter().any(|vi| match &vi.data {
                VariantData::Tuple(types) => types.iter().any(is_complex),
                VariantData::Struct(fields) => fields.iter().any(|(_, ty)| is_complex(ty)),
                VariantData::Unit => false,
            });
        }
    }
    false
}

/// Formats values via `snprintf` into a heap-allocated Expo string
/// (8-byte bit-length header followed by the character payload).
///
/// `fmt` is the printf format string and `args` are the values to substitute.
/// Returns a pointer to the payload (just past the header).
pub fn snprintf_to_expo_string<'ctx>(
    c: &mut Compiler<'ctx>,
    fmt: &str,
    args: &[BasicMetadataValueEnum<'ctx>],
    label: &str,
) -> PointerValue<'ctx> {
    let snprintf = *c.functions.get("snprintf").expect("snprintf not declared");
    let malloc = *c.functions.get("malloc").expect("malloc not declared");
    let i32_ty = c.context.i32_type();
    let i64_ty = c.context.i64_type();
    let i8_ty = c.context.i8_type();
    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());

    let fmt_global = c
        .builder
        .build_global_string_ptr(fmt, &format!("{label}_fmt"))
        .unwrap();

    let mut size_args: Vec<BasicMetadataValueEnum> = vec![
        ptr_ty.const_null().into(),
        i32_ty.const_int(0, false).into(),
        fmt_global.as_pointer_value().into(),
    ];
    size_args.extend_from_slice(args);

    let needed = c
        .call(snprintf, &size_args, &format!("{label}_needed"))
        .unwrap()
        .into_int_value();

    let needed_i64 = c
        .builder
        .build_int_z_extend(needed, i64_ty, &format!("{label}_n64"))
        .unwrap();
    let alloc_size = c
        .builder
        .build_int_add(
            needed_i64,
            i64_ty.const_int(9, false),
            &format!("{label}_sz"),
        )
        .unwrap();
    let base_ptr = c
        .call(malloc, &[alloc_size.into()], &format!("{label}_base"))
        .unwrap()
        .into_pointer_value();

    let bit_length = c
        .builder
        .build_int_mul(
            needed_i64,
            i64_ty.const_int(8, false),
            &format!("{label}_bits"),
        )
        .unwrap();
    c.builder.build_store(base_ptr, bit_length).unwrap();

    let payload = unsafe {
        c.builder
            .build_in_bounds_gep(
                i8_ty,
                base_ptr,
                &[i64_ty.const_int(8, false)],
                &format!("{label}_pay"),
            )
            .unwrap()
    };

    let buf_size = c
        .builder
        .build_int_add(
            needed,
            i32_ty.const_int(1, false),
            &format!("{label}_bufsz"),
        )
        .unwrap();

    let mut write_args: Vec<BasicMetadataValueEnum> = vec![
        payload.into(),
        buf_size.into(),
        fmt_global.as_pointer_value().into(),
    ];
    write_args.extend_from_slice(args);
    c.call_void(snprintf, &write_args, &format!("{label}_write"));

    payload
}

// ---------------------------------------------------------------------------
// Enum format synthesis
// ---------------------------------------------------------------------------

fn synthesize_enum_format<'ctx>(c: &mut Compiler<'ctx>, enum_name: &str) -> Result<(), String> {
    let enum_type = *c
        .types
        .structs
        .get(enum_name)
        .ok_or_else(|| format!("unknown enum type: {enum_name}"))?;

    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
    let fn_type = ptr_ty.fn_type(&[enum_type.into()], false);
    let fn_name = format!("{enum_name}_format");
    let fn_val = c.module.add_function(&fn_name, fn_type, None);
    c.functions.insert(fn_name.clone(), fn_val);

    let saved_block = c.builder.get_insert_block();
    let entry = c.context.append_basic_block(fn_val, "entry");
    c.builder.position_at_end(entry);

    let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();

    let alloca = c.builder.build_alloca(enum_type, "enum_alloca").unwrap();
    c.builder.build_store(alloca, self_val).unwrap();
    let tag_ptr = c
        .builder
        .build_struct_gep(enum_type, alloca, 0, "tag_ptr")
        .unwrap();
    let tag = c
        .builder
        .build_load(c.context.i8_type(), tag_ptr, "tag")
        .unwrap()
        .into_int_value();

    let variants = c
        .type_ctx
        .types
        .get(enum_name)
        .and_then(|ti| ti.variants())
        .cloned()
        .unwrap_or_default();

    if variants.is_empty() {
        let fallback = c.create_string_global(enum_name.as_bytes(), "enum_fallback");
        c.builder.build_return(Some(&fallback)).unwrap();
    } else {
        let merge_bb = c.context.append_basic_block(fn_val, "merge");

        let mut cases: Vec<(
            inkwell::values::IntValue<'ctx>,
            inkwell::basic_block::BasicBlock<'ctx>,
        )> = Vec::new();
        let mut incoming: Vec<(PointerValue<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
            Vec::new();

        for (i, vi) in variants.iter().enumerate() {
            let bb = c
                .context
                .append_basic_block(fn_val, &format!("v_{}", vi.name));
            cases.push((c.context.i8_type().const_int(i as u64, false), bb));

            c.builder.position_at_end(bb);

            let str_ptr = match &vi.data {
                VariantData::Unit => {
                    c.create_string_global(vi.name.as_bytes(), &format!("vn_{}", vi.name))
                }
                VariantData::Tuple(types) => {
                    if types.len() == 1 {
                        let payload_st = c.types.get_variant_payload_type(enum_name, &vi.name);

                        if let Some(payload_type) = payload_st {
                            let payload_ptr = c
                                .builder
                                .build_struct_gep(enum_type, alloca, 1, "payload_ptr")
                                .unwrap();
                            let payload_struct = c
                                .builder
                                .build_load(payload_type, payload_ptr, "payload")
                                .unwrap()
                                .into_struct_value();
                            let payload_val = c
                                .builder
                                .build_extract_value(payload_struct, 0, "payload_inner")
                                .unwrap();
                            let payload_str = call_format(c, payload_val, &types[0])?;

                            concat_variant_name(c, &vi.name, payload_str)
                        } else {
                            c.create_string_global(vi.name.as_bytes(), &format!("vn_{}", vi.name))
                        }
                    } else {
                        c.create_string_global(vi.name.as_bytes(), &format!("vn_{}", vi.name))
                    }
                }
                VariantData::Struct(_) => {
                    c.create_string_global(vi.name.as_bytes(), &format!("vn_{}", vi.name))
                }
            };

            incoming.push((str_ptr, c.builder.get_insert_block().unwrap()));
            c.builder.build_unconditional_branch(merge_bb).unwrap();
        }

        let default_bb = c.context.append_basic_block(fn_val, "default");
        c.builder.position_at_end(default_bb);
        let fallback = c.create_string_global(b"<unknown>", "unknown_variant");
        c.builder.build_unconditional_branch(merge_bb).unwrap();
        incoming.push((fallback, default_bb));

        c.builder.position_at_end(entry);
        c.builder.build_switch(tag, default_bb, &cases).unwrap();

        c.builder.position_at_end(merge_bb);
        let phi = c.builder.build_phi(ptr_ty, "result").unwrap();
        for (val, bb) in &incoming {
            phi.add_incoming(&[(val, *bb)]);
        }
        c.builder.build_return(Some(&phi.as_basic_value())).unwrap();
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}

fn concat_variant_name<'ctx>(
    c: &mut Compiler<'ctx>,
    variant_name: &str,
    payload_str: PointerValue<'ctx>,
) -> PointerValue<'ctx> {
    let fmt = format!("{variant_name}(%s)");
    snprintf_to_expo_string(c, &fmt, &[payload_str.into()], "vn")
}

// ---------------------------------------------------------------------------
// Struct format synthesis
// ---------------------------------------------------------------------------

fn synthesize_struct_format<'ctx>(c: &mut Compiler<'ctx>, struct_name: &str) -> Result<(), String> {
    let struct_type = *c
        .types
        .structs
        .get(struct_name)
        .ok_or_else(|| format!("unknown struct type: {struct_name}"))?;

    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
    let fn_type = ptr_ty.fn_type(&[struct_type.into()], false);
    let fn_name = format!("{struct_name}_format");
    let fn_val = c.module.add_function(&fn_name, fn_type, None);
    c.functions.insert(fn_name.clone(), fn_val);

    let saved_block = c.builder.get_insert_block();
    let entry = c.context.append_basic_block(fn_val, "entry");
    c.builder.position_at_end(entry);

    let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();

    let fields: Vec<(String, Type)> = c
        .type_ctx
        .types
        .get(struct_name)
        .and_then(|ti| ti.fields())
        .cloned()
        .unwrap_or_default();

    if fields.is_empty() {
        let s = c.create_string_global(struct_name.as_bytes(), "struct_fmt_empty");
        c.builder.build_return(Some(&s)).unwrap();
    } else {
        let alloca = c.builder.build_alloca(struct_type, "sf_alloca").unwrap();
        c.builder.build_store(alloca, self_val).unwrap();

        let mut field_strs: Vec<PointerValue<'ctx>> = Vec::new();
        for (i, (_, field_type)) in fields.iter().enumerate() {
            let field_ptr = c
                .builder
                .build_struct_gep(struct_type, alloca, i as u32, &format!("f_{i}"))
                .unwrap();
            let field_llvm_type = struct_type.get_field_type_at_index(i as u32).unwrap();
            let field_val = c
                .builder
                .build_load(field_llvm_type, field_ptr, &format!("fv_{i}"))
                .unwrap();
            field_strs.push(call_format(c, field_val, field_type)?);
        }

        let mut fmt_string = format!("{struct_name}{{");
        for (i, (name, _)) in fields.iter().enumerate() {
            if i > 0 {
                fmt_string.push_str(", ");
            }
            fmt_string.push_str(name);
            fmt_string.push_str(": %s");
        }
        fmt_string.push('}');

        let args: Vec<inkwell::values::BasicMetadataValueEnum> =
            field_strs.iter().map(|s| (*s).into()).collect();
        let payload = snprintf_to_expo_string(c, &fmt_string, &args, "sf");
        c.builder.build_return(Some(&payload)).unwrap();
    }

    if let Some(bb) = saved_block {
        c.builder.position_at_end(bb);
    }
    Ok(())
}
