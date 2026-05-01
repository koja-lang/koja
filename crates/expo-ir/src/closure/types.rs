//! Type closure: register every reachable monomorphized struct and
//! enum instantiation in [`crate::IRProgram`].
//!
//! Walks every function body's AST looking for [`expo_ast::ast::ExprKind::StructConstruction`]
//! and [`expo_ast::ast::ExprKind::EnumConstruction`] nodes whose
//! `resolved_type` carries non-empty `type_args`. Each such site
//! triggers a planner call ([`crate::lower::monomorphize::monomorphize_struct`]
//! or [`crate::lower::monomorphize::monomorphize_enum`]) which appends
//! the resulting decl to [`crate::IRProgram`].
//!
//! Newly inserted decls reference more types via their resolved field
//! / variant payloads. A worklist iterates until no new decls are
//! added, ensuring nested generics like `Box<Pair<Int>>` register both
//! `Box<Pair<Int>>` and `Pair<Int>`.
//!
//! Idempotency comes from the planners' `if program.contains_*` early
//! return, which also bounds the worklist.

use std::collections::HashSet;
use std::sync::OnceLock;

use expo_ast::ast::{Expr, ExprKind, Function};
use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::context::{TypeContext, VariantData};
use expo_typecheck::types::{Type, mangle_name};

use crate::closure::visit::visit_function_exprs;
use crate::identity::MonomorphizedTypeIdentifier;
use crate::lower::ctx::LowerCtx;
use crate::lower::monomorphize::{monomorphize_enum, monomorphize_struct};
use crate::program::{IRFunctionKind, IRProgram};
use crate::{FnLowerState, TypeLayouts};

/// A safety bound on worklist iterations. The planners' idempotency
/// guarantees termination, but cap iterations defensively in case a
/// future change introduces a non-idempotent path.
const MAX_WORKLIST_ITERATIONS: usize = 1024;

/// Run the type closure: discover every reachable generic struct/enum
/// instantiation in `program`'s function bodies and register it via
/// the LLVM-free planners. Repeats over freshly-registered decls'
/// sub-types until fixpoint.
pub fn run(program: &mut IRProgram, type_ctx: &TypeContext) -> Result<(), String> {
    let mut pending = collect_pending_from_bodies(program);
    let mut iteration = 0;
    while !pending.is_empty() {
        iteration += 1;
        if iteration > MAX_WORKLIST_ITERATIONS {
            return Err(format!(
                "type closure pass exceeded {MAX_WORKLIST_ITERATIONS} worklist iterations; \
                 suspect a non-idempotent planner or a runaway recursive instantiation"
            ));
        }
        pending = drain_pending(program, type_ctx, &pending)?;
    }
    Ok(())
}

/// Walk every callable body in `program`, collecting the initial set
/// of generic struct/enum instantiations referenced by the source.
fn collect_pending_from_bodies(program: &IRProgram) -> Vec<Pending> {
    let mut pending: Vec<Pending> = Vec::new();
    let mut seen: HashSet<MonomorphizedTypeIdentifier> = HashSet::new();
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

/// Drain the current `pending` queue: monomorphize each entry, then
/// inspect the freshly-registered decls' field / variant types for
/// nested generics. Returns the next round of pending instantiations.
fn drain_pending(
    program: &mut IRProgram,
    type_ctx: &TypeContext,
    pending: &[Pending],
) -> Result<Vec<Pending>, String> {
    let mut next_pending: Vec<Pending> = Vec::new();
    let mut seen: HashSet<MonomorphizedTypeIdentifier> = HashSet::new();
    for entry in pending {
        let Some(registered) = monomorphize_pending(program, type_ctx, entry)? else {
            continue;
        };
        collect_from_decl(program, &registered, &mut next_pending, &mut seen);
    }
    Ok(next_pending)
}

/// Plan the monomorphization for one pending entry, returning the
/// mangled name when a fresh decl was added. Discriminates
/// struct vs enum at planning time via the `TypeContext` lookup.
fn monomorphize_pending(
    program: &mut IRProgram,
    type_ctx: &TypeContext,
    entry: &Pending,
) -> Result<Option<MonomorphizedTypeIdentifier>, String> {
    let ctx = empty_lower_ctx(type_ctx);
    let is_enum = type_ctx
        .get_type(&entry.id)
        .is_some_and(|info| info.is_enum());
    if is_enum {
        monomorphize_enum(&ctx, program, &entry.id, &entry.type_args)
    } else {
        monomorphize_struct(&ctx, program, &entry.id, &entry.type_args)
    }
}

/// Inspect every type referenced by a freshly-registered decl
/// (`mangled`) and add any nested generic instantiations to `pending`.
fn collect_from_decl(
    program: &IRProgram,
    mangled: &MonomorphizedTypeIdentifier,
    pending: &mut Vec<Pending>,
    seen: &mut HashSet<MonomorphizedTypeIdentifier>,
) {
    if let Some(decl) = program.structs.get(mangled) {
        for (_, ty) in &decl.fields {
            collect_from_type(ty, pending, seen);
        }
    } else if let Some(decl) = program.enums.get(mangled) {
        for (_, variant) in &decl.variants {
            collect_from_variant(variant, pending, seen);
        }
    }
}

fn collect_from_variant(
    variant: &VariantData,
    pending: &mut Vec<Pending>,
    seen: &mut HashSet<MonomorphizedTypeIdentifier>,
) {
    match variant {
        VariantData::Struct(fields) => {
            for (_, ty) in fields {
                collect_from_type(ty, pending, seen);
            }
        }
        VariantData::Tuple(types) => {
            for ty in types {
                collect_from_type(ty, pending, seen);
            }
        }
        VariantData::Unit => {}
    }
}

/// Inspect an `Expr` for a generic struct/enum construction and
/// record the instantiation in `pending` (skipping ones already in
/// `seen`).
fn collect_from_expr(
    expr: &Expr,
    pending: &mut Vec<Pending>,
    seen: &mut HashSet<MonomorphizedTypeIdentifier>,
) {
    let resolved = expr.resolved_type.as_ref();
    if matches!(
        &expr.kind,
        ExprKind::StructConstruction { .. } | ExprKind::EnumConstruction { .. }
    ) && let Some(ty) = resolved
    {
        collect_from_type(ty, pending, seen);
    }
}

/// Walk a `Type`, registering every generic `Type::Named { type_args }`
/// it transitively contains. Nested types (e.g. `Box<Pair<Int>>`)
/// register both the outer and inner instantiations.
fn collect_from_type(
    ty: &Type,
    pending: &mut Vec<Pending>,
    seen: &mut HashSet<MonomorphizedTypeIdentifier>,
) {
    match ty {
        Type::Indirect(inner) | Type::Pointer(inner) => collect_from_type(inner, pending, seen),
        Type::Named {
            identifier,
            type_args,
        } if !type_args.is_empty() => {
            let mangled = MonomorphizedTypeIdentifier::new(mangle_name(identifier, type_args));
            if seen.insert(mangled) {
                pending.push(Pending {
                    id: identifier.clone(),
                    type_args: type_args.clone(),
                });
            }
            for arg in type_args {
                collect_from_type(arg, pending, seen);
            }
        }
        Type::Function {
            params,
            return_type,
        } => {
            for param in params {
                collect_from_type(&param.ty, pending, seen);
            }
            collect_from_type(return_type, pending, seen);
        }
        Type::Union(members) => {
            for member in members {
                collect_from_type(member, pending, seen);
            }
        }
        Type::Error
        | Type::Named { .. }
        | Type::Parameter(_)
        | Type::Primitive(_)
        | Type::Unit
        | Type::Unknown => {}
    }
}

/// Build a [`LowerCtx`] suitable for the planners. The closure pass
/// runs outside any function-lowering context, so it uses a default
/// [`FnLowerState`] and an empty [`TypeLayouts`]. Both are static
/// `OnceLock`s because the planners only read them.
///
/// Shared with [`super::functions`] so both sub-walks dispatch
/// planners through the same minimal context.
pub(super) fn empty_lower_ctx(type_ctx: &TypeContext) -> LowerCtx<'_> {
    static EMPTY_FN_STATE: OnceLock<FnLowerState> = OnceLock::new();
    static EMPTY_LAYOUTS: OnceLock<TypeLayouts> = OnceLock::new();

    let fn_state = EMPTY_FN_STATE.get_or_init(FnLowerState::new);
    let layouts = EMPTY_LAYOUTS.get_or_init(TypeLayouts::new);
    LowerCtx {
        closure_site_path: None,
        fn_lower: fn_state,
        layouts,
        locals: fn_state,
        package: None,
        type_ctx,
    }
}

/// Borrow the AST body off a callable [`IRFunctionKind`], or `None`
/// for kinds that don't carry one (extern, intrinsic, main entry,
/// thunk).
fn func_ast_of(kind: &IRFunctionKind) -> Option<&Function> {
    match kind {
        IRFunctionKind::Free { func_ast, .. } | IRFunctionKind::Method { func_ast, .. } => {
            Some(func_ast)
        }
        _ => None,
    }
}

/// One pending instantiation discovered by the walk.
struct Pending {
    id: TypeIdentifier,
    type_args: Vec<Type>,
}
