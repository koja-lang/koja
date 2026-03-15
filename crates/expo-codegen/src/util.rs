//! Shared utilities: integer literal parsing and printf format-specifier
//! selection for LLVM codegen.

use inkwell::values::{BasicValueEnum, IntValue, PointerValue};

use crate::compiler::Compiler;

/// Parses an integer literal string, handling `0x`/`0b` prefixes and `_`
/// separators.
pub fn parse_int_literal(s: &str) -> Result<i64, String> {
    let clean: String = s.chars().filter(|c| *c != '_').collect();
    if let Some(hex) = clean
        .strip_prefix("0x")
        .or_else(|| clean.strip_prefix("0X"))
    {
        i64::from_str_radix(hex, 16).map_err(|_| format!("invalid hex integer: {s}"))
    } else if let Some(bin) = clean
        .strip_prefix("0b")
        .or_else(|| clean.strip_prefix("0B"))
    {
        i64::from_str_radix(bin, 2).map_err(|_| format!("invalid binary integer: {s}"))
    } else {
        clean
            .parse()
            .map_err(|_| format!("integer literals cannot exceed {}", i64::MAX))
    }
}

/// Returns the `printf` format specifier (`%d`, `%lld`, `%f`, `%s`) for an
/// LLVM value based on its type.
pub fn printf_format_spec(val: &BasicValueEnum<'_>) -> Result<&'static str, String> {
    if val.is_int_value() {
        let width = val.into_int_value().get_type().get_bit_width();
        Ok(match width {
            32 => "%d",
            64 => "%lld",
            _ => "%d",
        })
    } else if val.is_float_value() {
        Ok("%f")
    } else if val.is_pointer_value() {
        Ok("%s")
    } else {
        Err("unsupported type for printf format".to_string())
    }
}

/// Emits an LLVM `select` that picks the global string `"true"` or `"false"`
/// based on an `i1` value, returning the chosen pointer.
pub fn bool_to_string_ptr<'ctx>(c: &mut Compiler<'ctx>, val: IntValue<'ctx>) -> PointerValue<'ctx> {
    let true_str = c
        .builder
        .build_global_string_ptr("true", "bool_true")
        .unwrap();
    let false_str = c
        .builder
        .build_global_string_ptr("false", "bool_false")
        .unwrap();
    c.builder
        .build_select(
            val,
            true_str.as_pointer_value(),
            false_str.as_pointer_value(),
            "bool_str",
        )
        .unwrap()
        .into_pointer_value()
}
