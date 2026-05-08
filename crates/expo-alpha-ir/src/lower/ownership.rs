//! Ownership classification at lowering time. Decides whether a
//! freshly-bound value is `Owned` (heap storage the slot must `free`
//! at scope exit) or `Unowned` (literal global, primitive copy, or
//! borrow). Ports v1's `expo_ir::lower::ownership` model onto alpha's
//! IR vocabulary.
//!
//! Two helpers, one per origin site:
//! - [`ownership_for_expr`] classifies an assignment-RHS expression
//!   given its IR-typed result.
//! - [`ownership_for_param`] classifies a parameter-promotion slot
//!   given the parameter's `PassMode`.
//!
//! Today every alpha-lowerable expression resolves to `Unowned`
//! because no heap-allocating expression has been wired in yet
//! (`<>` concat, `<<>>` binary literal, interpolated strings,
//! `Receive` are all feature-gapped in lowering). The
//! heap-producing arms remain so the next slice activates them by
//! ungapping the matching IR lowerers.
//!
//! **Pre-filter at the classifier** rather than v1's
//! "uniform-Owned then filter at drop emission" model: we only stamp
//! `Owned` on slots whose IR type can hold heap storage. `move c:
//! Int32` resolves to `Unowned` because `Int32` is a stack type.
//! Drop emission then only ever sees `Owned` slots that genuinely
//! need a `free`, which keeps the IR clean of no-op `DropLocal`
//! instructions.

use expo_ast::ast::{BinOp, Expr, ExprKind, PassMode, StringPart};

use crate::ownership::Ownership;
use crate::types::IRType;

/// Classify the ownership of an assignment-RHS expression. Returns
/// [`Ownership::Owned`] for heap-producing constructs and
/// [`Ownership::Unowned`] for literals / borrows / primitive copies.
pub(super) fn ownership_for_expr(expr: &Expr, value_type: &IRType) -> Ownership {
    if !is_heap_type(value_type) {
        return Ownership::Unowned;
    }
    match &expr.kind {
        ExprKind::Binary {
            op: BinOp::Concat, ..
        } => Ownership::Owned,
        ExprKind::BinaryLiteral { .. } => Ownership::Owned,
        ExprKind::Receive { .. } => Ownership::Owned,
        ExprKind::String { parts, .. } if parts.iter().any(is_interpolation) => Ownership::Owned,
        _ => Ownership::Unowned,
    }
}

/// Classify a parameter slot's ownership at promotion time. `move`
/// params (`move c: T`, `move self`) own their value when `T` is a
/// heap type; default-borrow and copy-mode (closure-capture-resolved)
/// slots never own. Stack-typed parameters always resolve to
/// [`Ownership::Unowned`] regardless of `mode` — `move c: Int32` is
/// a no-op semantically.
pub(super) fn ownership_for_param(mode: PassMode, ty: &IRType) -> Ownership {
    if !is_heap_type(ty) {
        return Ownership::Unowned;
    }
    match mode {
        PassMode::Move => Ownership::Owned,
        PassMode::Borrow | PassMode::Copy => Ownership::Unowned,
    }
}

/// Heap-allocated IR types: `String` today; `Binary` and `Bits`
/// extend this when the next slice ships them. Struct / enum
/// variants stay stack-allocated until they gain heap-typed fields,
/// at which point they migrate here too.
fn is_heap_type(ty: &IRType) -> bool {
    matches!(ty, IRType::String)
}

fn is_interpolation(part: &StringPart) -> bool {
    matches!(part, StringPart::Interpolation { .. })
}
