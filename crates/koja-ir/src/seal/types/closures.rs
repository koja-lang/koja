//! Operand types for the closure family: `CallClosure`,
//! `ClosureEquals`, `LoadCapture`, `LoadCaptureOf`, and `MakeClosure`.

use crate::function::{FunctionKind, IRSymbol};
use crate::seal::seal_panic;
use crate::types::{IRType, ValueId};

use super::{TypeScope, registered, require_same_type};

/// The callee must be a function value whose params and return type
/// agree with the instruction's cached `param_types` and `result_ty`.
pub(super) fn seal_call_closure(
    args: &[ValueId],
    callee: ValueId,
    param_types: &[IRType],
    result_ty: &IRType,
    scope: &TypeScope<'_>,
) {
    let owner = scope.owner;
    let IRType::Function { params, ret } = scope.value_type(callee) else {
        seal_panic(&format!(
            "{owner} CallClosure callee `{callee}` is not a function"
        ));
    };
    scope.require_argument_types(args, params, "CallClosure");
    require_same_type(ret, result_ty, &format!("{owner} CallClosure result type"));
    if param_types != params {
        seal_panic(&format!(
            "{owner} CallClosure param_types {param_types:?} disagree with callee \
             function type {params:?}"
        ));
    }
}

pub(super) fn seal_closure_equals(lhs: ValueId, rhs: ValueId, ty: &IRType, scope: &TypeScope<'_>) {
    if !matches!(ty, IRType::Function { .. }) {
        seal_panic(&format!(
            "{} ClosureEquals operand type `{ty:?}` is not a function type",
            scope.owner
        ));
    }
    scope.require_value_type(lhs, ty, "ClosureEquals lhs");
    scope.require_value_type(rhs, ty, "ClosureEquals rhs");
}

/// A `LoadCapture` must read the type its slot holds in the
/// enclosing closure's `env_layout`. Stray loads in non-closure
/// functions and out-of-range indexes panic in
/// `crate::seal::closures::seal_closure_ops`, so this pass only
/// types the in-range slot.
pub(super) fn seal_load_capture(capture_index: u32, ty: &IRType, scope: &TypeScope<'_>) {
    let Some(function) = scope.function else {
        return;
    };
    let (FunctionKind::Closure { env_layout }
    | FunctionKind::DropClosureGlue { env_layout }
    | FunctionKind::EqClosureGlue { env_layout }) = &function.kind
    else {
        return;
    };
    if let Some(expected) = env_layout.get(capture_index as usize) {
        require_same_type(ty, expected, &format!("{} LoadCapture type", scope.owner));
    }
}

/// The other closure must share the glue's own function type, so
/// its env has the glue's `env_layout`.
pub(super) fn seal_load_capture_of(
    capture_index: u32,
    closure: ValueId,
    ty: &IRType,
    scope: &TypeScope<'_>,
) {
    let Some(function) = scope.function else {
        return;
    };
    let FunctionKind::EqClosureGlue { env_layout } = &function.kind else {
        return;
    };
    let [other] = function.params.as_slice() else {
        return;
    };
    scope.require_value_type(closure, &other.ty, "LoadCaptureOf closure");
    if let Some(expected) = env_layout.get(capture_index as usize) {
        require_same_type(ty, expected, &format!("{} LoadCaptureOf type", scope.owner));
    }
}

/// The captures must match the body's `env_layout` and `ty` must be
/// the function type the body's signature spells out. A non-closure
/// body kind panics in `crate::seal::closures::seal_closure_ops`.
pub(super) fn seal_make_closure(
    body: &IRSymbol,
    captures: &[ValueId],
    ty: &IRType,
    scope: &TypeScope<'_>,
) {
    let target = registered(scope.declarations.function(body), "function", body);
    let FunctionKind::Closure { env_layout } = &target.kind else {
        return;
    };
    scope.require_argument_types(captures, env_layout, "MakeClosure");
    let expected = IRType::Function {
        params: target.params.iter().map(|param| param.ty.clone()).collect(),
        ret: Box::new(target.return_type.clone()),
    };
    require_same_type(ty, &expected, &format!("{} MakeClosure type", scope.owner));
}
