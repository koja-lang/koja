//! AST-side substitution helpers for monomorphization.
//!
//! [`substitute_in_function`] walks every [`ResolvedType`] slot
//! reachable from a function's body and rewrites it via
//! [`super::substitute_resolved_type`]. Mono drives this on a cloned
//! [`Function`] before re-lowering, so the body sees concrete
//! resolutions everywhere a `TypeParam` previously stood.
//!
//! [`substitute_signature`] does the same for a [`FunctionSignature`]
//! (params and return type), yielding the substituted signature
//! [`crate::lower::package::lower_function_inner`] needs.

use koja_ast::ast::{Expr, ExprKind, Function, LValue, Pattern};
use koja_ast::identifier::{GlobalRegistryId, ResolvedType};
use koja_ast::visit_mut::{self, VisitorMut};
use koja_typecheck::{FunctionSignature, ResolvedParam};

use super::substitute_resolved_type;

/// Substitute every [`ResolvedType`] reachable from `function`'s body
/// in place. Caller is responsible for cloning before substituting if
/// the original needs to stay intact (mono always clones).
pub(super) fn substitute_in_function(
    function: &mut Function,
    args: &[ResolvedType],
    owner: GlobalRegistryId,
) {
    let Some(body) = function.body.as_mut() else {
        return;
    };
    visit_mut::walk_body_mut(&mut Substituter { args, owner }, body);
}

/// Clone `signature` with every `params[].ty` and `return_type`
/// rewritten via [`substitute_resolved_type`]. Used by mono to feed
/// [`crate::lower::package::lower_function_inner`] a concrete shape
/// without mutating the registry-owned template.
pub(super) fn substitute_signature(
    signature: &FunctionSignature,
    args: &[ResolvedType],
    owner: GlobalRegistryId,
) -> FunctionSignature {
    FunctionSignature {
        declared_fallible: signature.declared_fallible,
        dispatch: signature.dispatch,
        params: signature
            .params
            .iter()
            .map(|param| ResolvedParam {
                name: param.name.clone(),
                ty: substitute_resolved_type(&param.ty, args, owner),
            })
            .collect(),
        return_type: substitute_resolved_type(&signature.return_type, args, owner),
        impl_args: signature
            .impl_args
            .iter()
            .map(|ty| substitute_resolved_type(ty, args, owner))
            .collect(),
    }
}

/// Rewrites the four [`ResolvedType`] slots a function body carries.
/// Those are `Expr.resolution`, the `type_args` on calls and method
/// calls, the head type of a multi-segment assignment target, and
/// the resolved type of a typed-binding pattern.
struct Substituter<'a> {
    args: &'a [ResolvedType],
    owner: GlobalRegistryId,
}

impl Substituter<'_> {
    fn rewrite(&self, slot: &mut ResolvedType) {
        *slot = substitute_resolved_type(slot, self.args, self.owner);
    }
}

impl VisitorMut for Substituter<'_> {
    /// A multi-segment target (`self.field = ...`) carries the head's
    /// type, which can be generic (a field of type `T` on the
    /// enclosing struct). Single-segment targets carry `None`.
    fn visit_lvalue_mut(&mut self, lvalue: &mut LValue) {
        if let Some(head) = lvalue.head_resolved_type.as_mut() {
            self.rewrite(head);
        }
    }

    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        self.rewrite(&mut expr.resolution);
        if let ExprKind::Call { type_args, .. } | ExprKind::MethodCall { type_args, .. } =
            &mut expr.kind
        {
            for ty in type_args {
                self.rewrite(ty);
            }
        }
        visit_mut::walk_expr_mut(self, expr);
    }

    /// Only [`Pattern::TypedBinding`] carries a generic-bearing
    /// resolution. `match` arms on a union subject and `receive` arms
    /// bind through it with annotations such as `xs: List<T>` or
    /// `((), Option<ReplyTo<R>>)`, and a raw `T` left there would leak
    /// into `resolved_type_to_ir_type` on re-lower.
    fn visit_pattern_mut(&mut self, pattern: &mut Pattern) {
        if let Pattern::TypedBinding {
            resolved_type: Some(ty),
            ..
        } = pattern
        {
            self.rewrite(ty);
        }
        visit_mut::walk_pattern_mut(self, pattern);
    }
}
