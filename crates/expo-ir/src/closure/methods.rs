//! Method closure: register every reachable user-defined generic
//! impl-method instantiation in [`crate::IRProgram`].
//!
//! Walks every callable body's AST looking for [`expo_ast::ast::ExprKind::MethodCall`]
//! sites whose receiver type is a generic instance. Each such site
//! triggers a planner call ([`crate::lower::monomorphize::monomorphize_impl_method`])
//! which appends the resulting [`crate::IRFunction`] (kind = `Method`)
//! to [`crate::IRProgram`].
//!
//! Skipped (deferred to codegen's lazy fallback for now):
//!
//! - Stdlib intrinsic types (`List`, `Map`, `Set`, `Ref`, `ReplyTo`,
//!   `CPtr`). Their methods are emitted directly by the backend
//!   without an IR decl; the closure pass would mis-register them as
//!   user `Method` IRFunctions.
//! - Receivers without a generic [`Type::Named`] resolved type (the
//!   call's monomorphization needs concrete `type_args`).
//! - Method-level type parameters (e.g. `map<U>`). Inferring them
//!   would mirror [`crate::lower::calls::infer_function_type_args`];
//!   left for a follow-up.

use std::collections::HashSet;

use expo_ast::ast::{Expr, ExprKind, Function};
use expo_typecheck::context::TypeContext;
use expo_typecheck::types::{Type, mangle_name};

use crate::closure::visit::visit_function_exprs;
use crate::identity::FunctionIdentifier;
use crate::lower::monomorphize::monomorphize_impl_method;
use crate::program::{IRFunctionKind, IRProgram};

/// Stdlib intrinsic base types. Their methods are emitted directly by
/// the backend (no IR decl). The closure pass leaves them to codegen's
/// existing lazy path.
const STDLIB_INTRINSIC_BASES: &[&str] = &["CPtr", "List", "Map", "Ref", "ReplyTo", "Set"];

/// Run the method closure over `program`, registering every reachable
/// user-defined generic impl-method instantiation.
pub fn run(program: &mut IRProgram, type_ctx: &TypeContext) -> Result<(), String> {
    let pending = collect_pending_from_bodies(program);
    for entry in pending {
        let ctx = super::types::empty_lower_ctx(type_ctx);
        monomorphize_impl_method(
            &ctx,
            program,
            &entry.base_type,
            &entry.method_name,
            &entry.type_args,
            &[],
        )?;
    }
    Ok(())
}

/// Walk every callable body in `program`, collecting the set of
/// generic impl-method instantiations the source actually invokes.
/// Deduplicates by mangled symbol.
fn collect_pending_from_bodies(program: &IRProgram) -> Vec<Pending> {
    let mut pending: Vec<Pending> = Vec::new();
    let mut seen: HashSet<FunctionIdentifier> = HashSet::new();
    for id in &program.function_order {
        let Some(function) = program.functions.get(id) else {
            continue;
        };
        let Some(func_ast) = func_ast_of(&function.kind) else {
            continue;
        };
        visit_function_exprs(func_ast, |expr| {
            collect_from_expr(expr, &mut pending, &mut seen);
        });
    }
    pending
}

/// Inspect an `Expr` for a generic method-call site and record the
/// instantiation in `pending` (skipping ones already in `seen`).
fn collect_from_expr(
    expr: &Expr,
    pending: &mut Vec<Pending>,
    seen: &mut HashSet<FunctionIdentifier>,
) {
    let ExprKind::MethodCall {
        receiver, method, ..
    } = &expr.kind
    else {
        return;
    };
    let Some(Type::Named {
        identifier,
        type_args,
    }) = receiver.resolved_type.as_ref()
    else {
        return;
    };
    if type_args.is_empty() {
        return;
    }
    if STDLIB_INTRINSIC_BASES.contains(&identifier.name.as_str()) {
        return;
    }
    let mangled_type = mangle_name(identifier, type_args);
    let mangled = FunctionIdentifier::new(format!("{mangled_type}_{method}"));
    if seen.insert(mangled) {
        pending.push(Pending {
            base_type: identifier.name.clone(),
            method_name: method.clone(),
            type_args: type_args.clone(),
        });
    }
}

/// Borrow the AST body off a callable [`IRFunctionKind`], or `None`
/// for kinds that don't carry one.
fn func_ast_of(kind: &IRFunctionKind) -> Option<&Function> {
    match kind {
        IRFunctionKind::Free { func_ast, .. } | IRFunctionKind::Method { func_ast, .. } => {
            Some(func_ast)
        }
        _ => None,
    }
}

/// One pending method instantiation discovered by the walk.
struct Pending {
    base_type: String,
    method_name: String,
    type_args: Vec<Type>,
}
