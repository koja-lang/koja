//! Shared LLVM emission helpers for string interpolation.
//!
//! Both the AST-level [`crate::expr::compile_string`] (still reached
//! through `IRInstruction::Stub`-nested AST expressions, e.g.
//! interpolated strings in closure bodies) and the typed-IR
//! [`crate::control::instructions::emit_string_format`] executor
//! produce a `printf`-style `(fmt_string, interp_values)` pair from
//! their respective walkers (`StringPart` vs `StringFormatPart`),
//! then pump it through an identical
//! `snprintf`-twice + `malloc(needed + 9)` + bit-length-store
//! sequence to land a heap-allocated, length-prefixed string buffer.
//!
//! [`assemble_interpolated_string`] hosts that shared assembly so the
//! two part-walkers can collapse to thin format-spec dispatchers.
//! [`interp_arg_for_value`] hosts the per-hole `printf` vs `call_format`
//! choice so the walkers don't drift apart on what counts as a
//! "plain printf-able" value.

use expo_ir::identity::FunctionIdentifier;
use expo_typecheck::types::Type;
use inkwell::AddressSpace;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};

use crate::compiler::Compiler;
use crate::debug::call_format;
use crate::util::printf_format_spec;

/// One interpolation hole's contribution to the format string +
/// argument list: the `printf` conversion specifier (e.g. `"%d"`,
/// `"%s"`) and the runtime value to feed `snprintf`.
pub(crate) struct InterpArg<'ctx> {
    pub format_spec: &'static str,
    pub value: BasicValueEnum<'ctx>,
}

/// Decide the per-hole `printf` conversion for an interpolated value.
///
/// Plain integers / pointers / floats route through `printf` directly
/// via [`printf_format_spec`]. Booleans and aggregates (struct values
/// like options/results, enums, etc.) detour through `call_format`,
/// which produces a heap `String` we then splice in with `%s`.
pub(crate) fn interp_arg_for_value<'ctx>(
    compiler: &mut Compiler<'ctx>,
    value: BasicValueEnum<'ctx>,
    expo_type: &Type,
) -> Result<InterpArg<'ctx>, String> {
    let is_bool = value.is_int_value() && value.into_int_value().get_type().get_bit_width() == 1;
    let is_plain_printf =
        !value.is_struct_value() && !is_bool && printf_format_spec(&value).is_ok();

    if is_plain_printf {
        Ok(InterpArg {
            format_spec: printf_format_spec(&value).unwrap(),
            value,
        })
    } else {
        let str_ptr = call_format(compiler, value, expo_type)?;
        Ok(InterpArg {
            format_spec: "%s",
            value: str_ptr.into(),
        })
    }
}

/// Append a literal string fragment to the in-progress `printf`
/// format string, escaping any `%` so it doesn't get interpreted as
/// a conversion specifier.
pub(crate) fn append_literal_to_format(fmt_string: &mut String, value: &str) {
    for ch in value.chars() {
        if ch == '%' {
            fmt_string.push_str("%%");
        } else {
            fmt_string.push(ch);
        }
    }
}

/// Run the shared `snprintf`-twice + `malloc` + length-prefix
/// sequence given an already-built `(fmt_string, interp_values)` pair.
/// Returns the payload pointer (past the 8-byte length prefix), shaped
/// to match the rest of the runtime's `String` representation.
///
/// The two walkers (AST `compile_string` and IR `emit_string_format`)
/// stay separate because they iterate over different AST/IR types,
/// but everything from "we have the format and args" onward lives
/// here.
pub(crate) fn assemble_interpolated_string<'ctx>(
    c: &mut Compiler<'ctx>,
    fmt_string: &str,
    interp_values: &[BasicValueEnum<'ctx>],
) -> Result<BasicValueEnum<'ctx>, String> {
    let snprintf = *c
        .functions
        .get(&FunctionIdentifier::new("snprintf"))
        .ok_or("snprintf not declared")?;

    let fmt_global = c
        .builder
        .build_global_string_ptr(fmt_string, "interp_fmt")
        .unwrap();
    let fmt_ptr = fmt_global.as_pointer_value();

    let i32_type = c.context.i32_type();
    let ptr_type = c.context.ptr_type(AddressSpace::default());
    let null_ptr = ptr_type.const_null();
    let zero = i32_type.const_int(0, false);

    let mut size_args: Vec<BasicValueEnum> = vec![null_ptr.into(), zero.into(), fmt_ptr.into()];
    size_args.extend_from_slice(interp_values);
    let size_args_meta: Vec<BasicMetadataValueEnum> =
        size_args.iter().map(|v| (*v).into()).collect();

    let needed = c
        .call(snprintf, &size_args_meta, "interp_len")
        .ok_or("snprintf did not return a value")?
        .into_int_value();

    let one = i32_type.const_int(1, false);
    let buf_size = c.builder.build_int_add(needed, one, "buf_size").unwrap();

    let malloc_fn = *c
        .functions
        .get(&FunctionIdentifier::new("malloc"))
        .ok_or("malloc not declared")?;
    let i64_type = c.context.i64_type();
    let i8_type = c.context.i8_type();
    let needed_i64 = c
        .builder
        .build_int_z_extend(needed, i64_type, "needed_i64")
        .unwrap();
    let alloc_size = c
        .builder
        .build_int_add(needed_i64, i64_type.const_int(9, false), "interp_alloc_sz")
        .unwrap();
    let base_ptr: PointerValue<'ctx> = c
        .call(malloc_fn, &[alloc_size.into()], "interp_base")
        .ok_or("malloc did not return a value")?
        .into_pointer_value();

    let bit_length = c
        .builder
        .build_int_mul(needed_i64, i64_type.const_int(8, false), "bit_length")
        .unwrap();
    c.builder.build_store(base_ptr, bit_length).unwrap();

    let payload = unsafe {
        c.builder
            .build_in_bounds_gep(
                i8_type,
                base_ptr,
                &[i64_type.const_int(8, false)],
                "interp_payload",
            )
            .unwrap()
    };

    let mut write_args: Vec<BasicValueEnum> = vec![payload.into(), buf_size.into(), fmt_ptr.into()];
    write_args.extend_from_slice(interp_values);
    let write_args_meta: Vec<BasicMetadataValueEnum> =
        write_args.iter().map(|v| (*v).into()).collect();

    c.call_void(snprintf, &write_args_meta, "interp_write");

    Ok(payload.into())
}
