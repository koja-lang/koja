//! LLVM-free utility helpers shared between lowering passes.

/// Parses an integer literal string, handling `0x`/`0b` prefixes and `_`
/// separators.
pub fn parse_int_literal(string: &str) -> Result<i64, String> {
    let cleaned: String = string.chars().filter(|c| *c != '_').collect();
    if let Some(hex) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        i64::from_str_radix(hex, 16).map_err(|_| format!("invalid hex integer: {string}"))
    } else if let Some(bin) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        i64::from_str_radix(bin, 2).map_err(|_| format!("invalid binary integer: {string}"))
    } else {
        cleaned
            .parse()
            .map_err(|_| format!("integer literals cannot exceed {}", i64::MAX))
    }
}
