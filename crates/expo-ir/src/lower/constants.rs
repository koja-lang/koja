//! Lowering for constant initializers: parses literals and identifies
//! enum/struct construction shapes into [`crate::resolved::constants`]
//! values that the constant emission path can consume mechanically.

use expo_ast::ast::{EnumConstructionData, ExprKind, Literal, StringPart};

use crate::lower::LowerCtx;
use crate::resolved::constants::{ResolvedConst, ResolvedConstEnum};
use crate::util::parse_int_literal;

/// Resolves a constant expression to its semantic kind by parsing literals
/// and identifying enum/struct construction shapes.
pub fn resolve_const(kind: &ExprKind) -> Option<ResolvedConst> {
    match kind {
        ExprKind::Literal {
            value: Literal::Bool(b),
            ..
        } => Some(ResolvedConst::Bool(*b)),
        ExprKind::Literal {
            value: Literal::Float(s),
            ..
        } => {
            let v: f64 = s.parse().ok()?;
            Some(ResolvedConst::Float(v))
        }
        ExprKind::Literal {
            value: Literal::Int(s),
            ..
        } => {
            let v = parse_int_literal(s).ok()?;
            Some(ResolvedConst::Int(v))
        }
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data: EnumConstructionData::Unit,
            ..
        } => Some(ResolvedConst::EnumVariant {
            enum_name: type_path.join("."),
            variant: variant.clone(),
        }),
        ExprKind::String { parts, .. } => {
            let mut combined = String::new();
            for part in parts {
                if let StringPart::Literal { value, .. } = part {
                    combined.push_str(value);
                }
            }
            Some(ResolvedConst::String(combined))
        }
        ExprKind::StructConstruction {
            type_path, fields, ..
        } => Some(ResolvedConst::Struct {
            fields: fields.clone(),
            struct_name: type_path.join("."),
        }),
        _ => None,
    }
}

/// Looks up the tag for a unit enum variant used in a constant initializer.
pub fn resolve_const_enum(
    ctx: &LowerCtx<'_>,
    enum_name: &str,
    variant: &str,
) -> Option<ResolvedConstEnum> {
    let tag = ctx.layouts.variant_index(enum_name, variant)?;
    Some(ResolvedConstEnum { tag })
}
