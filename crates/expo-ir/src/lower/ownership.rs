//! Ownership classification at lowering time.
//!
//! [`ownership_for_expr`] decides whether a freshly-bound let
//! expression yields an owned-heap value (interpolated string,
//! mailbox-received binary, concat result, struct value, list/map/
//! set literal) or an unowned borrow (string literal pointer,
//! primitive, integer constant). The result is baked into the
//! [`crate::IRInstruction::StoreLocal`] emitted at the assignment
//! site so codegen's drop pass knows which variables it must free.
//!
//! Lifted from `expo-codegen::stmt::ownership_for_expr` in Phase 4g
//! Slice 1 -- the decision is pure-semantic (no LLVM types involved)
//! so it belongs in `expo-ir`.

use expo_ast::ast::{BinOp, Expr, ExprKind, StringPart};
use expo_typecheck::types::{Primitive, Type};

use crate::ownership::Ownership;

/// Determine ownership semantics for an assigned value based on its
/// expression kind and the bound variable's declared type. Concat
/// (`<>`), interpolated strings, mailbox `Receive`s, and arbitrary
/// non-string-literal values all produce owned heap allocations;
/// plain string literals and pre-existing primitive borrows do not.
pub fn ownership_for_expr(expr: &Expr, assigned_type: &Type) -> Ownership {
    if is_concat_expr(expr) {
        return Ownership::Owned;
    }
    if matches!(
        assigned_type,
        Type::Primitive(Primitive::Binary) | Type::Primitive(Primitive::Bits)
    ) {
        return match &expr.kind {
            ExprKind::BinaryLiteral { .. } | ExprKind::Receive { .. } => Ownership::Owned,
            _ => Ownership::Unowned,
        };
    }
    if !matches!(assigned_type, Type::Primitive(Primitive::String)) {
        return Ownership::Owned;
    }
    match &expr.kind {
        ExprKind::Receive { .. } => Ownership::Owned,
        ExprKind::String { parts, .. } => {
            let interpolated = parts
                .iter()
                .any(|part| matches!(part, StringPart::Interpolation { .. }));
            if interpolated {
                Ownership::Owned
            } else {
                Ownership::Unowned
            }
        }
        _ => Ownership::Unowned,
    }
}

/// True when `expr` is a binary concat (`<>`) operation. Concat
/// always produces a freshly-allocated string / binary, so the
/// resulting binding is [`Ownership::Owned`].
fn is_concat_expr(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::Binary {
            op: BinOp::Concat,
            ..
        }
    )
}
