//! Method closure: register every reachable user-defined generic
//! impl-method instantiation in [`crate::IRProgram`].
//!
//! Walks every callable body's AST looking for [`expo_ast::ast::ExprKind::MethodCall`]
//! sites whose receiver type is a generic instance. Each such site
//! triggers a planner call ([`crate::lower::monomorphize::monomorphize_impl_method`])
//! which appends the resulting [`crate::IRFunction`] (kind = `Method`)
//! to [`crate::IRProgram`].
//!
//! Method-level type parameters (e.g. `map<U>`) are inferred via
//! [`crate::lower::inference::infer_method_type_args`] using the
//! call args and the receiver's substituted type-args; the dedupe
//! key includes them so `List<Int>.map<String>` and
//! `List<Int>.map<Bool>` track as distinct instantiations.
//!
//! Skipped (deferred to codegen's lazy fallback):
//!
//! - Stdlib intrinsic types (`List`, `Map`, `Set`, `Ref`, `ReplyTo`,
//!   `CPtr`). Their methods are emitted directly by the backend
//!   without an IR decl; the closure pass would mis-register them as
//!   user `Method` IRFunctions.
//! - Receivers without a generic [`Type::Named`] resolved type (the
//!   call's monomorphization needs concrete `type_args`).
//! - Method-level type-args that don't fully infer from the call
//!   shape (returns `Err`); those sites stay on the lazy path until
//!   inference can be tightened further.

use std::collections::{HashMap, HashSet};

use expo_ast::ast::{Arg, Expr, ExprKind, Function};
use expo_typecheck::context::TypeContext;
use expo_typecheck::types::{Type, mangle_method_suffix, mangle_name, substitute};

use crate::closure::visit::visit_function_exprs;
use crate::identity::FunctionIdentifier;
use crate::lower::inference::infer_method_type_args;
use crate::lower::monomorphize::monomorphize_impl_method;
use crate::program::{IRFunctionKind, IRProgram};

/// Stdlib intrinsic base types. Their methods are emitted directly by
/// the backend (no IR decl). The closure pass leaves them to codegen's
/// existing lazy path.
const STDLIB_INTRINSIC_BASES: &[&str] = &["CPtr", "List", "Map", "Ref", "ReplyTo", "Set"];

/// Run the method closure over `program`, registering every reachable
/// user-defined generic impl-method instantiation.
pub fn run(program: &mut IRProgram, type_ctx: &TypeContext) -> Result<(), String> {
    let pending = collect_pending_from_bodies(program, type_ctx);
    for entry in pending {
        let ctx = super::types::empty_lower_ctx(type_ctx);
        monomorphize_impl_method(
            &ctx,
            program,
            &entry.base_type,
            &entry.method_name,
            &entry.type_args,
            &entry.method_type_args,
        )?;
    }
    Ok(())
}

/// Walk every callable body in `program`, collecting the set of
/// generic impl-method instantiations the source actually invokes.
/// Deduplicates by mangled symbol.
fn collect_pending_from_bodies(program: &IRProgram, type_ctx: &TypeContext) -> Vec<Pending> {
    let mut pending: Vec<Pending> = Vec::new();
    let mut seen: HashSet<FunctionIdentifier> = HashSet::new();
    for id in &program.function_order {
        let Some(function) = program.functions.get(id) else {
            continue;
        };
        let Some((func_ast, subst)) = func_ast_of(&function.kind) else {
            continue;
        };
        visit_function_exprs(func_ast, |expr| {
            collect_from_expr(expr, type_ctx, subst, &mut pending, &mut seen);
        });
    }
    pending
}

/// Inspect an `Expr` for a generic method-call site and record the
/// instantiation in `pending` (skipping ones already in `seen`).
///
/// When the called method has its own `<U>` type params,
/// [`infer_method_type_args`] derives them from the call args and
/// the receiver's substituted type-args. The dedupe key includes
/// the method-level args so `List<Int>.map<String>` and
/// `List<Int>.map<Bool>` track separately.
fn collect_from_expr(
    expr: &Expr,
    type_ctx: &TypeContext,
    subst: &HashMap<String, Type>,
    pending: &mut Vec<Pending>,
    seen: &mut HashSet<FunctionIdentifier>,
) {
    let ExprKind::MethodCall {
        receiver,
        method,
        args,
    } = &expr.kind
    else {
        return;
    };
    let Some(receiver_type) = receiver.resolved_type.as_ref() else {
        return;
    };
    let Type::Named {
        identifier,
        type_args,
    } = substitute(receiver_type, subst)
    else {
        return;
    };
    if type_args.is_empty() {
        return;
    }
    if STDLIB_INTRINSIC_BASES.contains(&identifier.name.as_str()) {
        return;
    }
    let method_type_args =
        match resolve_method_type_args(type_ctx, subst, &identifier.name, method, &type_args, args)
        {
            Ok(args) => args,
            Err(()) => return,
        };
    let mangled_type = mangle_name(&identifier, &type_args);
    let suffix = mangle_method_suffix(method, &method_type_args);
    let mangled = FunctionIdentifier::new(format!("{mangled_type}_{suffix}"));
    if seen.insert(mangled) {
        pending.push(Pending {
            base_type: identifier.name.clone(),
            method_name: method.clone(),
            type_args,
            method_type_args,
        });
    }
}

/// Resolve the method-level `<U>` type args for `(base_type, method)`.
/// Returns `Ok(vec![])` for non-generic methods, `Ok(args)` for
/// generic methods whose args fully infer, and `Err(())` when
/// inference fails or yields `Unknown` for any slot. `Err` is
/// silent -- the caller defers the site to codegen's lazy fallback,
/// matching the previous skip behavior.
fn resolve_method_type_args(
    type_ctx: &TypeContext,
    subst: &HashMap<String, Type>,
    base_type: &str,
    method: &str,
    receiver_type_args: &[Type],
    args: &[Arg],
) -> Result<Vec<Type>, ()> {
    let ctx = super::types::empty_lower_ctx(type_ctx);
    let inferred =
        infer_method_type_args(&ctx, &|_| None, base_type, method, receiver_type_args, args)
            .map_err(|_| ())?;
    let substituted: Vec<Type> = inferred.iter().map(|t| substitute(t, subst)).collect();
    if substituted.iter().any(|t| matches!(t, Type::Unknown)) {
        return Err(());
    }
    Ok(substituted)
}

/// Borrow the AST body and type substitution off a callable
/// [`IRFunctionKind`], or `None` for kinds that don't carry one.
fn func_ast_of(kind: &IRFunctionKind) -> Option<(&Function, &HashMap<String, Type>)> {
    match kind {
        IRFunctionKind::Free {
            func_ast, subst, ..
        }
        | IRFunctionKind::Method {
            func_ast, subst, ..
        } => Some((func_ast, subst)),
        _ => None,
    }
}

/// One pending method instantiation discovered by the walk.
struct Pending {
    base_type: String,
    method_name: String,
    type_args: Vec<Type>,
    method_type_args: Vec<Type>,
}
