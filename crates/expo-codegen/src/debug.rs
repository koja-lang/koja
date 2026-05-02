//! Debug protocol support: invokes `{Type}_format` on a value to get its
//! string representation, and provides the `snprintf` -> Expo-string
//! helper used by primitive intrinsic format functions and string
//! interpolation.
//!
//! Format function bodies for user-defined structs / enums are
//! synthesized at the AST level by `expo-preprocess::derive::derive_debug`
//! and emitted through the normal codegen path -- this module no longer
//! performs lazy LLVM-side synthesis for them.

use expo_ast::identifier::Package;
use expo_ir::identity::FunctionIdentifier;
use expo_ir::lower::naming::method_symbol_prefix;
use expo_typecheck::types::{Primitive, Type, named};
use inkwell::AddressSpace;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};

use crate::compiler::Compiler;
use crate::intrinsics::{emit_primitive_intrinsic, is_primitive_intrinsic, type_display_name};

/// Calls `{Type}_format(val)` and returns the resulting string pointer.
///
/// Primitive types (`Int`, `Bool`, `Float`, ...) lazily synthesize their
/// intrinsic format function the first time they're requested. Every
/// other type relies on a `format` declared by an explicit `impl Debug`
/// (user-written or auto-derived by `expo-preprocess`); this helper does
/// not synthesize those bodies.
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
        && is_primitive_intrinsic(&fn_name)
    {
        let parameter_type = val.get_type();
        let pointer_type = compiler.context.ptr_type(AddressSpace::default());
        let function_type = pointer_type.fn_type(&[parameter_type.into()], false);
        let function_value = compiler.module.add_function(&fn_name, function_type, None);
        compiler.register_intrinsic(
            FunctionIdentifier::new(fn_name.clone()),
            function_value,
            &type_name,
            "format",
        );
        emit_primitive_intrinsic(compiler, &fn_name)?;
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
