//! Function closure: register every reachable monomorphized free
//! function instantiation in [`crate::IRProgram`].
//!
//! Walks every callable body's AST looking for [`expo_ast::ast::ExprKind::Call`]
//! sites whose callee resolves to a generic free function (one with
//! type parameters, stashed in `generic_fn_asts` by codegen). Each
//! such site infers the concrete type-argument vector via
//! [`crate::lower::calls::infer_function_type_args`] and dispatches
//! the planner [`crate::lower::monomorphize::monomorphize_function`].
//!
//! Skipped (deferred to codegen's lazy fallback for now):
//!
//! - Sites whose argument expressions lack a populated `resolved_type`
//!   (typecheck didn't produce enough information for inference).
//! - Sites where inference yields a type-arg slot of [`Type::Unknown`]
//!   (some type parameters can't be inferred from arguments alone).
//! - Sites inside the user `fn main` body. The synthesized
//!   [`crate::IRFunctionKind::MainEntry`] kind carries no AST body, so
//!   the function-order walk above has nothing to inspect for `main`;
//!   generic free-function calls written directly in `fn main` (e.g.
//!   `identity(42)` in `tests/lang/generics/generics.expo`) reach
//!   codegen's lazy fallback. Closing this gap requires either
//!   teaching `MainEntry` to carry the body or threading the files
//!   slice through [`super::closure_program`]; both are larger
//!   architectural changes than this walk.
//!
//! The outer fixpoint loop in [`super::closure_program`] re-runs the
//! type closure after this walk so any newly-monomorphized function's
//! body has a chance to register the generic struct/enum
//! instantiations it references.

use std::collections::{HashMap, HashSet};

use expo_ast::ast::{Expr, ExprKind, Function};
use expo_typecheck::context::TypeContext;
use expo_typecheck::types::{Type, substitute};

use crate::closure::visit::visit_function_exprs;
use crate::identity::FunctionIdentifier;
use crate::lower::calls::infer_function_type_args;
use crate::lower::monomorphize::monomorphize_function;
use crate::program::{IRFunctionKind, IRProgram};

/// Run the function closure over `program`, registering every
/// reachable generic free-function instantiation.
pub fn run(
    program: &mut IRProgram,
    type_ctx: &TypeContext,
    generic_fn_asts: &HashMap<String, Function>,
) -> Result<(), String> {
    let pending = collect_pending_from_bodies(program, type_ctx, generic_fn_asts);
    for entry in pending {
        let ctx = super::types::empty_lower_ctx(type_ctx);
        monomorphize_function(
            &ctx,
            program,
            generic_fn_asts,
            &entry.name,
            &entry.type_args,
        )?;
    }
    Ok(())
}

/// Walk every callable body in `program`, collecting the set of
/// generic free-function instantiations the source actually calls.
/// Deduplicates by mangled symbol.
fn collect_pending_from_bodies(
    program: &IRProgram,
    type_ctx: &TypeContext,
    generic_fn_asts: &HashMap<String, Function>,
) -> Vec<Pending> {
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
            collect_from_expr(
                expr,
                type_ctx,
                subst,
                generic_fn_asts,
                &mut pending,
                &mut seen,
            );
        });
    }
    pending
}

/// Inspect an `Expr` for a generic call site and record the
/// instantiation in `pending` (skipping ones already in `seen`).
/// Bails on missing `resolved_type` on any argument or on inference
/// failure: those sites stay on codegen's lazy fallback for now.
///
/// Each argument's `resolved_type` is filtered through the enclosing
/// function's `subst` so a call inside `foo<T>` whose arg type is
/// `T` becomes the concrete instantiation type before inference.
fn collect_from_expr(
    expr: &Expr,
    type_ctx: &TypeContext,
    subst: &HashMap<String, Type>,
    generic_fn_asts: &HashMap<String, Function>,
    pending: &mut Vec<Pending>,
    seen: &mut HashSet<FunctionIdentifier>,
) {
    let ExprKind::Call { callee, args } = &expr.kind else {
        return;
    };
    let ExprKind::Ident { name } = &callee.kind else {
        return;
    };
    if !generic_fn_asts.contains_key(name) {
        return;
    }
    let arg_types: Option<Vec<Type>> = args
        .iter()
        .map(|arg| {
            arg.value
                .resolved_type
                .as_ref()
                .map(|t| substitute(t, subst))
        })
        .collect();
    let Some(arg_types) = arg_types else {
        return;
    };
    let Ok(type_args) = infer_function_type_args(type_ctx, name, &arg_types) else {
        return;
    };
    if type_args.iter().any(|t| matches!(t, Type::Unknown)) {
        return;
    }
    let mangled = FunctionIdentifier::new(mangled_name(name, &type_args));
    if seen.insert(mangled) {
        pending.push(Pending {
            name: name.clone(),
            type_args,
        });
    }
}

/// Mirror of [`expo_typecheck::types::mangle_method_suffix`] without
/// re-importing it at every call site.
fn mangled_name(name: &str, type_args: &[Type]) -> String {
    expo_typecheck::types::mangle_method_suffix(name, type_args)
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

/// One pending free-function instantiation discovered by the walk.
struct Pending {
    name: String,
    type_args: Vec<Type>,
}
