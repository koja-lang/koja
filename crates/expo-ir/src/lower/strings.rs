//! Lowering for string-related operator decisions, e.g. choosing between
//! the binary and string concatenation strategies for `<>`.

use expo_ast::ast::{Expr, ExprKind};
use expo_typecheck::types::{Primitive, Type};

use crate::lower::LowerCtx;
use crate::resolved::strings::ResolvedConcatKind;

/// Decides whether a `<>` expression with the given left operand should
/// use the binary or string concatenation strategy. Identifier types are
/// looked up via `var_type`, so this helper stays free of any
/// backend-specific variable map.
pub fn resolve_concat_kind(
    _ctx: &LowerCtx<'_>,
    left: &Expr,
    var_type: impl Fn(&str) -> Option<Type>,
) -> ResolvedConcatKind {
    match concat_operand_type(left, var_type) {
        Type::Primitive(Primitive::Binary) | Type::Primitive(Primitive::Bits) => {
            ResolvedConcatKind::Binary
        }
        _ => ResolvedConcatKind::String,
    }
}

/// Best-effort type inference for an operand of `<>`. Identifiers consult
/// `var_type`; `BinaryLiteral` is `Binary`; everything else falls back to
/// `String`.
fn concat_operand_type(expr: &Expr, var_type: impl Fn(&str) -> Option<Type>) -> Type {
    if let ExprKind::Ident { name, .. } = &expr.kind
        && let Some(ty) = var_type(name)
    {
        return ty;
    }
    if matches!(expr.kind, ExprKind::BinaryLiteral { .. }) {
        return Type::Primitive(Primitive::Binary);
    }
    Type::Primitive(Primitive::String)
}
