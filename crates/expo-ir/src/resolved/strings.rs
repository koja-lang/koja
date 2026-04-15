//! Resolved string expression types.

use expo_ast::ast::StringPart;

/// Whether a string expression is a plain literal or contains interpolation.
pub enum ResolvedString {
    /// Contains `#{}` interpolation -- must be compiled part-by-part.
    Interpolated,
    /// A plain string literal with no interpolation.
    Literal { value: String },
}

/// Pure decision function: inspects string parts to determine whether the
/// string contains interpolation or is a simple literal.
pub fn resolve_string(parts: &[StringPart]) -> ResolvedString {
    let has_interpolation = parts
        .iter()
        .any(|p| matches!(p, StringPart::Interpolation { .. }));

    if has_interpolation {
        return ResolvedString::Interpolated;
    }

    let mut combined = String::new();
    for part in parts {
        if let StringPart::Literal { value, .. } = part {
            combined.push_str(value);
        }
    }
    ResolvedString::Literal { value: combined }
}
