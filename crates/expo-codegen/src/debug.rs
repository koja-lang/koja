//! Debug protocol support: synthesizes `format` functions for enums and
//! structs, and provides `call_format` to invoke `{Type}_format` on any value.

use expo_ast::identifier::{Package, TypeIdentifier};
use expo_ir::identity::{FunctionIdentifier, VariantIdentifier};
use expo_ir::lower::debug::{
    format_fn_name, resolve_enum_format_info, resolve_format_kind, resolve_struct_format_info,
    resolve_type_id,
};
use expo_ir::lower::naming::method_symbol_prefix;
use expo_ir::resolved::debug::ResolvedFormatKind;
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{Primitive, Type, named};
use inkwell::AddressSpace;
use inkwell::basic_block::BasicBlock;
use inkwell::types::StructType;
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PointerValue, StructValue,
};

use crate::compiler::Compiler;
use crate::intrinsics::{emit_primitive_intrinsic, is_primitive_intrinsic, type_display_name};

/// Calls `{Type}_format(val)` and returns the resulting string pointer.
/// Synthesizes the format function on demand for enums and structs if it
/// doesn't already exist. Falls back to LLVM type inspection when the
/// Expo type is unknown.
pub fn call_format<'ctx>(
    compiler: &mut Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    expo_type: &Type,
) -> Result<PointerValue<'ctx>, String> {
    let resolved_type = if matches!(expo_type, Type::Unknown) {
        infer_type_from_llvm(val)
    } else {
        expo_type.clone()
    };

    let type_name = type_display_name(&resolved_type);
    // Named user/stdlib types carry a package through `Type::Named`; format
    // symbols are emitted in lockstep with `method_symbol_prefix` so the
    // synthesized function name matches what call sites generate. Falling
    // back to the bare name is only correct for primitives or unresolved
    // types (e.g. intrinsics).
    let resolved_id = match &resolved_type {
        Type::Named { identifier, .. } if identifier.package != Package::Unresolved => {
            Some(identifier.clone())
        }
        _ => None,
    };
    let fn_name = match &resolved_id {
        Some(id) => {
            let prefix = method_symbol_prefix(&id.package, &id.name);
            format!("{prefix}_format")
        }
        None => format!("{type_name}_format"),
    };

    if !compiler
        .functions
        .contains_key(&FunctionIdentifier::new(&fn_name))
    {
        let Some(kind) = resolve_format_kind(
            &compiler.lower_ctx(),
            resolved_id.as_ref(),
            &fn_name,
            &type_name,
            is_primitive_intrinsic,
        ) else {
            return Err(format!("no format function for type `{type_name}`"));
        };
        match kind {
            ResolvedFormatKind::Enum => {
                let id = resolved_id
                    .clone()
                    .map(Ok)
                    .unwrap_or_else(|| resolve_type_id(&compiler.lower_ctx(), &type_name))?;
                synthesize_enum_format(compiler, &id)?;
            }
            ResolvedFormatKind::Struct => {
                let id = resolved_id
                    .clone()
                    .map(Ok)
                    .unwrap_or_else(|| resolve_type_id(&compiler.lower_ctx(), &type_name))?;
                synthesize_struct_format(compiler, &id)?;
            }
            ResolvedFormatKind::PrimitiveIntrinsic => {
                let parameter_type = val.get_type();
                let pointer_type = compiler.context.ptr_type(AddressSpace::default());
                let function_type = pointer_type.fn_type(&[parameter_type.into()], false);
                let function_value = compiler.module.add_function(&fn_name, function_type, None);
                compiler
                    .functions
                    .insert(FunctionIdentifier::new(fn_name.clone()), function_value);
                emit_primitive_intrinsic(compiler, &fn_name)?;
            }
        }
    }

    let format_fn = *compiler
        .functions
        .get(&FunctionIdentifier::new(&fn_name))
        .ok_or_else(|| format!("no format function for type `{type_name}`"))?;

    compiler
        .call(format_fn, &[val.into()], "fmt_result")
        .map(|value| value.into_pointer_value())
        .ok_or_else(|| format!("{fn_name} did not return a value"))
}

/// Pre-synthesizes `{Type}_format` functions for all user-defined structs and
/// enums. Call after types are registered and functions are declared, but
/// before function bodies are compiled.
pub fn synthesize_all_formats<'ctx>(compiler: &mut Compiler<'ctx>) -> Result<(), String> {
    let types: Vec<(TypeIdentifier, bool)> = compiler
        .type_ctx
        .types
        .iter()
        .filter(|(_, type_info)| {
            (type_info.is_struct() || type_info.is_enum()) && type_info.type_params.is_empty()
        })
        .map(|(id, type_info)| (id.clone(), type_info.is_enum()))
        .collect();

    for (id, is_enum) in &types {
        let fn_name = format_fn_name(id);
        if compiler
            .functions
            .contains_key(&FunctionIdentifier::new(&fn_name))
        {
            continue;
        }
        if has_unsynthesizable_fields(compiler, id) {
            continue;
        }
        if *is_enum {
            synthesize_enum_format(compiler, id)?;
        } else {
            synthesize_struct_format(compiler, id)?;
        }
    }
    Ok(())
}

/// Formats values via `snprintf` into a heap-allocated Expo string
/// (8-byte bit-length header followed by the character payload).
///
/// `fmt` is the printf format string and `args` are the values to substitute.
/// Returns a pointer to the payload (just past the header).
pub fn snprintf_to_expo_string<'ctx>(
    compiler: &mut Compiler<'ctx>,
    fmt: &str,
    args: &[BasicMetadataValueEnum<'ctx>],
    label: &str,
) -> PointerValue<'ctx> {
    let snprintf = *compiler
        .functions
        .get(&FunctionIdentifier::new("snprintf"))
        .expect("snprintf not declared");
    let malloc = *compiler
        .functions
        .get(&FunctionIdentifier::new("malloc"))
        .expect("malloc not declared");
    let i32_type = compiler.context.i32_type();
    let i64_type = compiler.context.i64_type();
    let i8_type = compiler.context.i8_type();
    let pointer_type = compiler.context.ptr_type(AddressSpace::default());

    let fmt_global = compiler
        .builder
        .build_global_string_ptr(fmt, &format!("{label}_fmt"))
        .unwrap();

    let mut size_args: Vec<BasicMetadataValueEnum> = vec![
        pointer_type.const_null().into(),
        i32_type.const_int(0, false).into(),
        fmt_global.as_pointer_value().into(),
    ];
    size_args.extend_from_slice(args);

    let needed = compiler
        .call(snprintf, &size_args, &format!("{label}_needed"))
        .unwrap()
        .into_int_value();

    let needed_i64 = compiler
        .builder
        .build_int_z_extend(needed, i64_type, &format!("{label}_n64"))
        .unwrap();
    let alloc_size = compiler
        .builder
        .build_int_add(
            needed_i64,
            i64_type.const_int(9, false),
            &format!("{label}_sz"),
        )
        .unwrap();
    let base_ptr = compiler
        .call(malloc, &[alloc_size.into()], &format!("{label}_base"))
        .unwrap()
        .into_pointer_value();

    let bit_length = compiler
        .builder
        .build_int_mul(
            needed_i64,
            i64_type.const_int(8, false),
            &format!("{label}_bits"),
        )
        .unwrap();
    compiler.builder.build_store(base_ptr, bit_length).unwrap();

    let payload = unsafe {
        compiler
            .builder
            .build_in_bounds_gep(
                i8_type,
                base_ptr,
                &[i64_type.const_int(8, false)],
                &format!("{label}_pay"),
            )
            .unwrap()
    };

    let buf_size = compiler
        .builder
        .build_int_add(
            needed,
            i32_type.const_int(1, false),
            &format!("{label}_bufsz"),
        )
        .unwrap();

    let mut write_args: Vec<BasicMetadataValueEnum> = vec![
        payload.into(),
        buf_size.into(),
        fmt_global.as_pointer_value().into(),
    ];
    write_args.extend_from_slice(args);
    compiler.call_void(snprintf, &write_args, &format!("{label}_write"));

    payload
}

/// Synthesizes a `{Enum}_format(self) -> String` function that switch-cases
/// over the tag and returns a string representation of each variant.
fn synthesize_enum_format<'ctx>(
    compiler: &mut Compiler<'ctx>,
    id: &TypeIdentifier,
) -> Result<(), String> {
    let qualified = id.qualified_name();
    let enum_name = &id.name;
    let resolved = resolve_enum_format_info(&compiler.lower_ctx(), id);
    let synthesis = begin_synthesis(compiler, id, &resolved.function_name)?;

    let alloca = compiler
        .builder
        .build_alloca(synthesis.llvm_type, "enum_alloca")
        .unwrap();
    compiler
        .builder
        .build_store(alloca, synthesis.self_value)
        .unwrap();
    let tag_ptr = compiler
        .builder
        .build_struct_gep(synthesis.llvm_type, alloca, 0, "tag_ptr")
        .unwrap();
    let tag = compiler
        .builder
        .build_load(compiler.context.i8_type(), tag_ptr, "tag")
        .unwrap()
        .into_int_value();

    let variant_name_label = |compiler: &mut Compiler<'ctx>, name: &str| -> PointerValue<'ctx> {
        compiler.create_string_global(name.as_bytes(), &format!("vn_{name}"))
    };

    if resolved.variants.is_empty() {
        let fallback = compiler.create_string_global(enum_name.as_bytes(), "enum_fallback");
        compiler.builder.build_return(Some(&fallback)).unwrap();
    } else {
        let merge_block = compiler
            .context
            .append_basic_block(synthesis.function_value, "merge");

        let mut cases: Vec<(IntValue<'ctx>, BasicBlock<'ctx>)> = Vec::new();
        let mut incoming: Vec<(PointerValue<'ctx>, BasicBlock<'ctx>)> = Vec::new();

        for (i, variant_info) in resolved.variants.iter().enumerate() {
            let basic_block = compiler.context.append_basic_block(
                synthesis.function_value,
                &format!("v_{}", variant_info.name),
            );
            cases.push((
                compiler.context.i8_type().const_int(i as u64, false),
                basic_block,
            ));

            compiler.builder.position_at_end(basic_block);

            let str_ptr = match &variant_info.data {
                VariantData::Unit => variant_name_label(compiler, &variant_info.name),
                VariantData::Tuple(types) => {
                    if types.len() == 1 && !is_complex_type(&types[0]) {
                        let id = VariantIdentifier::new(&qualified, &variant_info.name);
                        let payload_struct_type = compiler.llvm_types.variant_payload(&id);

                        if let Some(payload_type) = payload_struct_type {
                            let payload_ptr = compiler
                                .builder
                                .build_struct_gep(synthesis.llvm_type, alloca, 1, "payload_ptr")
                                .unwrap();
                            let payload_struct = compiler
                                .builder
                                .build_load(payload_type, payload_ptr, "payload")
                                .unwrap()
                                .into_struct_value();
                            let payload_val = compiler
                                .builder
                                .build_extract_value(payload_struct, 0, "payload_inner")
                                .unwrap();
                            let payload_str = call_format(compiler, payload_val, &types[0])?;

                            concat_variant_name(compiler, &variant_info.name, payload_str)
                        } else {
                            variant_name_label(compiler, &variant_info.name)
                        }
                    } else {
                        variant_name_label(compiler, &variant_info.name)
                    }
                }
                VariantData::Struct(_) => variant_name_label(compiler, &variant_info.name),
            };

            incoming.push((str_ptr, compiler.builder.get_insert_block().unwrap()));
            compiler
                .builder
                .build_unconditional_branch(merge_block)
                .unwrap();
        }

        let default_block = compiler
            .context
            .append_basic_block(synthesis.function_value, "default");
        compiler.builder.position_at_end(default_block);
        let fallback = compiler.create_string_global(b"<unknown>", "unknown_variant");
        compiler
            .builder
            .build_unconditional_branch(merge_block)
            .unwrap();
        incoming.push((fallback, default_block));

        compiler
            .builder
            .position_at_end(synthesis.function_value.get_first_basic_block().unwrap());
        compiler
            .builder
            .build_switch(tag, default_block, &cases)
            .unwrap();

        compiler.builder.position_at_end(merge_block);
        let pointer_type = compiler.context.ptr_type(AddressSpace::default());
        let phi = compiler.builder.build_phi(pointer_type, "result").unwrap();
        for (val, basic_block) in &incoming {
            phi.add_incoming(&[(val, *basic_block)]);
        }
        compiler
            .builder
            .build_return(Some(&phi.as_basic_value()))
            .unwrap();
    }

    end_synthesis(compiler, synthesis.saved_block);
    Ok(())
}

/// Formats a variant with a payload as `VariantName(payload_str)`.
fn concat_variant_name<'ctx>(
    compiler: &mut Compiler<'ctx>,
    variant_name: &str,
    payload_str: PointerValue<'ctx>,
) -> PointerValue<'ctx> {
    let fmt = format!("{variant_name}(%s)");
    snprintf_to_expo_string(compiler, &fmt, &[payload_str.into()], "vn")
}

/// Synthesizes a `{Struct}_format(self) -> String` function that reads each
/// field, formats it, and returns `StructName{field: value, ...}`.
fn synthesize_struct_format<'ctx>(
    compiler: &mut Compiler<'ctx>,
    id: &TypeIdentifier,
) -> Result<(), String> {
    let struct_name = &id.name;
    let resolved = resolve_struct_format_info(&compiler.lower_ctx(), id);
    let synthesis = begin_synthesis(compiler, id, &resolved.function_name)?;

    let fields = &resolved.fields;

    if fields.is_empty() {
        let empty_label = compiler.create_string_global(struct_name.as_bytes(), "struct_fmt_empty");
        compiler.builder.build_return(Some(&empty_label)).unwrap();
    } else {
        let alloca = compiler
            .builder
            .build_alloca(synthesis.llvm_type, "sf_alloca")
            .unwrap();
        compiler
            .builder
            .build_store(alloca, synthesis.self_value)
            .unwrap();

        let mut field_strs: Vec<PointerValue<'ctx>> = Vec::new();
        for (i, (_, field_type)) in fields.iter().enumerate() {
            let field_ptr = compiler
                .builder
                .build_struct_gep(synthesis.llvm_type, alloca, i as u32, &format!("f_{i}"))
                .unwrap();
            let field_llvm_type = synthesis
                .llvm_type
                .get_field_type_at_index(i as u32)
                .unwrap();
            let field_val = compiler
                .builder
                .build_load(field_llvm_type, field_ptr, &format!("fv_{i}"))
                .unwrap();
            if is_complex_type(field_type) {
                field_strs.push(compiler.create_string_global(b"...", &format!("f_opaque_{i}")));
            } else {
                field_strs.push(call_format(compiler, field_val, field_type)?);
            }
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

        let args: Vec<BasicMetadataValueEnum> =
            field_strs.iter().map(|str_ptr| (*str_ptr).into()).collect();
        let payload = snprintf_to_expo_string(compiler, &fmt_string, &args, "sf");
        compiler.builder.build_return(Some(&payload)).unwrap();
    }

    end_synthesis(compiler, synthesis.saved_block);
    Ok(())
}

/// Shared state for the begin/end synthesis pattern used by both enum and
/// struct format synthesis.
struct SynthesisContext<'ctx> {
    function_value: FunctionValue<'ctx>,
    llvm_type: StructType<'ctx>,
    saved_block: Option<BasicBlock<'ctx>>,
    self_value: StructValue<'ctx>,
}

/// Looks up the LLVM struct type for `id`, creates a format function
/// `function_name(self) -> ptr`, saves the current insert block, and
/// positions the builder at the new function's entry block.
///
/// Uses the strict `TypeIdentifier`-keyed lookup so synthesis for the
/// canary's `alpha.Status` can't accidentally resolve to `beta.Status`
/// via bare-name fallback.
fn begin_synthesis<'ctx>(
    compiler: &mut Compiler<'ctx>,
    id: &TypeIdentifier,
    function_name: &str,
) -> Result<SynthesisContext<'ctx>, String> {
    let llvm_type = compiler
        .llvm_types
        .get_concrete(id)
        .ok_or_else(|| format!("unknown type: {id}"))?;

    let pointer_type = compiler.context.ptr_type(AddressSpace::default());
    let function_type = pointer_type.fn_type(&[llvm_type.into()], false);
    let function_value = compiler
        .module
        .add_function(function_name, function_type, None);
    compiler
        .functions
        .insert(FunctionIdentifier::new(function_name), function_value);

    let saved_block = compiler.builder.get_insert_block();
    let entry = compiler.context.append_basic_block(function_value, "entry");
    compiler.builder.position_at_end(entry);

    let self_value = function_value.get_nth_param(0).unwrap().into_struct_value();

    Ok(SynthesisContext {
        function_value,
        llvm_type,
        saved_block,
        self_value,
    })
}

/// Restores the builder to the insert block that was active before synthesis.
fn end_synthesis(compiler: &mut Compiler, saved_block: Option<BasicBlock>) {
    if let Some(block) = saved_block {
        compiler.builder.position_at_end(block);
    }
}

/// Infers an Expo [`Type`] from an LLVM value by inspecting its bit width
/// or struct name. Used as a fallback when the Expo type is unknown.
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
        let float_type = val.into_float_value().get_type();
        if float_type == float_type.get_context().f32_type() {
            Type::Primitive(Primitive::F32)
        } else {
            Type::Primitive(Primitive::F64)
        }
    } else if val.is_pointer_value() {
        Type::Primitive(Primitive::String)
    } else if val.is_struct_value() {
        let struct_type = val.into_struct_value().get_type();
        if let Some(name) = struct_type.get_name().and_then(|n| n.to_str().ok()) {
            named(name)
        } else {
            Type::Unknown
        }
    } else {
        Type::Unknown
    }
}

/// Returns `true` when a type is too complex for the auto-synthesized
/// `format` function (e.g. generics, indirects, pointers).
fn is_complex_type(expo_type: &Type) -> bool {
    match expo_type {
        Type::Indirect(_) | Type::Pointer(_) | Type::Unknown => true,
        Type::Named { type_args, .. } => !type_args.is_empty(),
        _ => false,
    }
}

/// Returns `true` if any field or variant payload contains a complex type
/// that the format synthesizer cannot handle.
fn has_unsynthesizable_fields(compiler: &Compiler, id: &TypeIdentifier) -> bool {
    if let Some(type_info) = compiler.type_ctx.get_type(id) {
        if let Some(fields) = type_info.fields() {
            return fields
                .iter()
                .any(|(_, field_type)| is_complex_type(field_type));
        }
        if let Some(variants) = type_info.variants() {
            return variants
                .iter()
                .any(|variant_info| match &variant_info.data {
                    VariantData::Tuple(types) => types.iter().any(is_complex_type),
                    VariantData::Struct(fields) => fields
                        .iter()
                        .any(|(_, field_type)| is_complex_type(field_type)),
                    VariantData::Unit => false,
                });
        }
    }
    false
}
