//! AST-side substitution helpers for monomorphization.
//!
//! [`substitute_in_function`] walks every [`ResolvedType`] slot
//! reachable from a function's body and rewrites it via
//! [`koja_typecheck::substitute`]. Mono drives this on a cloned
//! [`Function`] before re-lowering, so the body sees concrete
//! resolutions everywhere a `TypeParam` previously stood.
//!
//! [`substitute_signature`] does the same for a [`FunctionSignature`]
//! (params and return type), yielding the substituted signature
//! [`crate::lower::package::lower_function_inner`] needs.

use koja_ast::ast::{Expr, ExprKind, Function, LValue, Pattern};
use koja_ast::coercion::Coercion;
use koja_ast::identifier::ResolvedType;
use koja_ast::visit_mut::{self, VisitorMut};
use koja_typecheck::{FunctionSignature, GlobalRegistry, ResolvedParam, Substitution, substitute};

/// Substitute every [`ResolvedType`] reachable from `function`'s body
/// in place. Caller is responsible for cloning before substituting if
/// the original needs to stay intact (mono always clones).
pub(super) fn substitute_in_function(
    function: &mut Function,
    subst: &Substitution,
    registry: &GlobalRegistry,
) {
    let Some(body) = function.body.as_mut() else {
        return;
    };
    let mut substituter = Substituter { registry, subst };
    visit_mut::walk_body_mut(&mut substituter, body);
}

/// Clone `signature` with every `params[].ty` and `return_type`
/// rewritten via [`substitute`]. Used by mono to feed
/// [`crate::lower::package::lower_function_inner`] a concrete shape
/// without mutating the registry-owned template.
pub(super) fn substitute_signature(
    signature: &FunctionSignature,
    subst: &Substitution,
    registry: &GlobalRegistry,
) -> FunctionSignature {
    FunctionSignature {
        declared_fallible: signature.declared_fallible,
        dispatch: signature.dispatch,
        params: signature
            .params
            .iter()
            .map(|param| ResolvedParam {
                name: param.name.clone(),
                ty: substitute(&param.ty, subst, registry),
            })
            .collect(),
        return_type: substitute(&signature.return_type, subst, registry),
        impl_args: signature
            .impl_args
            .iter()
            .map(|ty| substitute(ty, subst, registry))
            .collect(),
    }
}

/// Rewrites the five [`ResolvedType`] slots a function body carries.
/// Those are `Expr.resolution`, the target type of `Expr.coercion`,
/// the `type_args` on calls and method calls, the head type of a
/// multi-segment assignment target, and the resolved type of a
/// typed-binding pattern.
struct Substituter<'a> {
    registry: &'a GlobalRegistry,
    subst: &'a Substitution,
}

impl Substituter<'_> {
    fn rewrite(&self, slot: &mut ResolvedType) {
        *slot = substitute(slot, self.subst, self.registry);
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

    /// A widening in a generic body records its target with the type
    /// parameter still in it, as in `Int | List<T>`. The lowering
    /// builds the union value from that target, so it must be
    /// concrete. The `match` has no catch-all arm, so a new
    /// [`Coercion`] variant does not compile until it has an arm here.
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        self.rewrite(&mut expr.resolution);
        if let Some(coercion) = expr.coercion.as_mut() {
            match coercion {
                Coercion::NumericWiden(target) | Coercion::UnionWiden(target) => {
                    self.rewrite(target)
                }
            }
        }
        if let ExprKind::Call { type_args, .. } | ExprKind::MethodCall { type_args, .. } =
            &mut expr.kind
        {
            for ty in type_args {
                self.rewrite(ty);
            }
        }
        visit_mut::walk_expr_mut(self, expr);
    }

    /// [`Pattern::TypedBinding`] and [`Pattern::UnionMember`] carry a
    /// generic-bearing resolution. `match` arms on a union subject
    /// and `receive` arms bind through a typed binding with
    /// annotations such as `xs: List<T>` or
    /// `((), Option<ReplyTo<R>>)`. A union member pattern holds the
    /// member it matches, such as `Option<T>`. A raw `T` left in
    /// either would leak into `resolved_type_to_ir_type` on re-lower.
    fn visit_pattern_mut(&mut self, pattern: &mut Pattern) {
        match pattern {
            Pattern::TypedBinding {
                resolved_type: Some(ty),
                ..
            }
            | Pattern::UnionMember {
                member_type: ty, ..
            } => self.rewrite(ty),
            _ => {}
        }
        visit_mut::walk_pattern_mut(self, pattern);
    }
}
