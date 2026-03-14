use inkwell::values::BasicValueEnum;

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
            1 | 32 => "%d",
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
