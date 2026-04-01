//! Receiver type resolution for dot-completion and signature help.
//!
//! Given a token before a `.` (like `socket`, `Socket`, or `self`), attempts
//! to determine the type name so we can look up methods and fields.

use expo_typecheck::context::TypeContext;

/// Attempts to resolve the type name of a receiver token.
///
/// - Uppercase tokens that match a known type are returned as-is (static context).
/// - `"self"` returns `None` (callers should handle via enclosing `impl` context).
/// - Lowercase tokens are resolved by scanning the source for `let name = Type.new(...)` or
///   `let name: Type = ...` patterns.
pub(crate) fn resolve_receiver_type(
    receiver: &str,
    source: &str,
    ctx: &TypeContext,
) -> Option<String> {
    if receiver == "self" {
        return None;
    }

    let first_char = receiver.chars().next()?;
    if first_char.is_uppercase() && ctx.types.contains_key(receiver) {
        return Some(receiver.to_string());
    }

    infer_variable_type(receiver, source, ctx)
}

/// Scans source for `let <name> = <Type>.new(...)` or `let <name>: <Type> = ...`
/// to infer a variable's type.
fn infer_variable_type(name: &str, source: &str, ctx: &TypeContext) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();

        // Pattern: `name = Type.new(` or `name = Type.new {`
        if let Some(rest) = trimmed
            .strip_prefix(name)
            .and_then(|s| s.trim_start().strip_prefix('='))
        {
            let rest = rest.trim_start();
            if let Some(type_name) = extract_constructor_type(rest, ctx) {
                return Some(type_name);
            }
        }

        // Pattern: `let name: Type = ...` or `name: Type = ...`
        let after_let = trimmed.strip_prefix("let ").unwrap_or(trimmed);
        if let Some(rest) = after_let
            .strip_prefix(name)
            .and_then(|s| s.trim_start().strip_prefix(':'))
        {
            let rest = rest.trim_start();
            let type_name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !type_name.is_empty() && ctx.types.contains_key(&type_name) {
                return Some(type_name);
            }
        }
    }
    None
}

/// Extracts a type name from a `Type.new(...)` or `Type.method(...)` pattern.
fn extract_constructor_type(text: &str, ctx: &TypeContext) -> Option<String> {
    let type_name: String = text
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if type_name.is_empty() || !type_name.chars().next()?.is_uppercase() {
        return None;
    }

    if ctx.types.contains_key(&type_name) {
        return Some(type_name);
    }

    None
}
