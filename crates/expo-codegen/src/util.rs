//! Shared codegen utilities: printf format-specifier selection and the
//! `bool -> "true"/"false"` string select. Pure-semantic helpers live in
//! [`expo_ir::util`].

use inkwell::values::{BasicValueEnum, IntValue, PointerValue};

use crate::compiler::Compiler;

/// Returns the `printf` format specifier (`%d`, `%lld`, `%f`, `%s`) for an
/// LLVM value based on its type.
pub fn printf_format_spec(value: &BasicValueEnum<'_>) -> Result<&'static str, String> {
    if value.is_int_value() {
        let width = value.into_int_value().get_type().get_bit_width();
        Ok(match width {
            32 => "%d",
            64 => "%lld",
            _ => "%d",
        })
    } else if value.is_float_value() {
        Ok("%f")
    } else if value.is_pointer_value() {
        Ok("%s")
    } else {
        Err("unsupported type for printf format".to_string())
    }
}

/// Emits an LLVM `select` that picks an Expo-format `String` (`"true"` or
/// `"false"`) based on an `i1` value, returning the chosen payload pointer.
/// The globals are length-prefixed so the result satisfies `String`'s
/// runtime contract (`byte_length`, `Fd.write`, etc.).
pub fn bool_to_string_ptr<'ctx>(
    compiler: &mut Compiler<'ctx>,
    value: IntValue<'ctx>,
) -> PointerValue<'ctx> {
    let true_str = compiler.create_string_global(b"true", "bool_true");
    let false_str = compiler.create_string_global(b"false", "bool_false");

    compiler
        .builder
        .build_select(value, true_str, false_str, "bool_str")
        .unwrap()
        .into_pointer_value()
}
