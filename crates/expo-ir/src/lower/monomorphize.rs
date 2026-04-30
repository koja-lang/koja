//! Monomorphization planners: pure semantic decision functions that
//! specialize generic structs, enums, free functions, and impl methods
//! for concrete type arguments and append the resulting declarations to
//! an [`IRProgram`].
//!
//! These functions never touch a backend — they only read from
//! [`LowerCtx`] and write to [`IRProgram`]. Backends (today: codegen's
//! `emit_ir_*` family) consume the resulting decls and perform their own
//! emission. The plan-then-emit split is what lets monomorphization run
//! `<'ctx>`-free, closing Phase 4b of the EXPOIR migration.
//!
//! Each planner returns:
//! - `Ok(Some(id))` if a new decl was appended (caller should emit),
//! - `Ok(None)` if the decl was already present (caller should skip),
//! - `Err(_)` on a semantic error.
//!
//! The mangled identity is stable across the planner's idempotency check
//! and what the backend emits, so the `Some(id)` return doubles as the
//! lookup key into [`IRProgram::structs`] / `enums` / `functions`.

use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{
    Type, build_substitution, mangle_method_suffix, mangle_name, substitute,
};

use crate::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier};
use crate::lower::LowerCtx;
use crate::lower::methods::resolve_method_signature;
use crate::lower::types::resolve_name_current;
use crate::program::{
    IREnum, IRFunction, IRFunctionKind, IRFunctionMeta, IRProgram, IRStruct, IRStructKind,
};

/// Plans a monomorphized struct: resolves field types under the
/// substitution, classifies the struct as user-defined or a stdlib
/// intrinsic, and appends an [`IRStruct`] to `program`.
///
/// Returns `Ok(Some(mangled))` when a new decl was added, `Ok(None)` when
/// the decl was already present.
pub fn monomorphize_struct(
    ctx: &LowerCtx<'_>,
    program: &mut IRProgram,
    id: &TypeIdentifier,
    type_args: &[Type],
) -> Result<Option<MonomorphizedTypeIdentifier>, String> {
    let mangled = MonomorphizedTypeIdentifier::new(mangle_name(id, type_args));
    if program.contains_struct(&mangled) {
        return Ok(None);
    }

    let name = id.name.as_str();

    // Stdlib intrinsics whose physical layout is fixed by the backend
    // (e.g. `List<T>` is always `{ ptr, length, capacity }`). Resolved
    // fields are populated to a canonical placeholder so future passes
    // can still walk struct decls uniformly; backends short-circuit on
    // `kind` to use their hard-coded layout.
    if id.is_std() {
        let intrinsic_kind = match name {
            "List" => Some(IRStructKind::StdList),
            "Map" | "Set" => Some(IRStructKind::StdHashtable),
            "Ref" => Some(IRStructKind::StdRef),
            "ReplyTo" => Some(IRStructKind::StdReplyTo),
            _ => None,
        };
        if let Some(kind) = intrinsic_kind {
            program.insert_struct(IRStruct {
                mangled: mangled.clone(),
                fields: Vec::new(),
                kind,
            });
            return Ok(Some(mangled));
        }
    }

    let info = ctx
        .type_ctx
        .get_type(id)
        .ok_or_else(|| format!("no struct info for generic struct `{name}`"))?;
    let fields = info
        .fields()
        .ok_or_else(|| format!("no struct info for generic struct `{name}`"))?;

    let subst = build_substitution(&info.type_params, type_args);

    let concrete_fields: Vec<(String, Type)> = fields
        .iter()
        .map(|(fname, fty)| (fname.clone(), substitute(fty, &subst)))
        .collect();

    program.insert_struct(IRStruct {
        mangled: mangled.clone(),
        fields: concrete_fields,
        kind: IRStructKind::User,
    });

    Ok(Some(mangled))
}

/// Plans a monomorphized enum: resolves each variant's payload types
/// under the substitution and appends an [`IREnum`] to `program`.
pub fn monomorphize_enum(
    ctx: &LowerCtx<'_>,
    program: &mut IRProgram,
    id: &TypeIdentifier,
    type_args: &[Type],
) -> Result<Option<MonomorphizedTypeIdentifier>, String> {
    let mangled = MonomorphizedTypeIdentifier::new(mangle_name(id, type_args));
    if program.contains_enum(&mangled) {
        return Ok(None);
    }

    let name = id.name.as_str();
    let info = ctx
        .type_ctx
        .get_type(id)
        .ok_or_else(|| format!("no enum info for generic enum `{name}`"))?;
    let variants = info
        .variants()
        .ok_or_else(|| format!("no enum info for generic enum `{name}`"))?;

    let subst = build_substitution(&info.type_params, type_args);

    let concrete_variants: Vec<(String, VariantData)> = variants
        .iter()
        .map(|vi| {
            let data = match &vi.data {
                VariantData::Unit => VariantData::Unit,
                VariantData::Tuple(types) => {
                    VariantData::Tuple(types.iter().map(|t| substitute(t, &subst)).collect())
                }
                VariantData::Struct(fields) => VariantData::Struct(
                    fields
                        .iter()
                        .map(|(n, t)| (n.clone(), substitute(t, &subst)))
                        .collect(),
                ),
            };
            (vi.name.clone(), data)
        })
        .collect();

    program.insert_enum(IREnum {
        mangled: mangled.clone(),
        variants: concrete_variants,
    });

    Ok(Some(mangled))
}

/// Plans a monomorphized free function: looks up the generic AST, builds
/// the type substitution, resolves param/return types, and appends an
/// [`IRFunction`] to `program`. The function body remains as raw AST
/// inside the decl; backend emission lowers it to LLVM.
pub fn monomorphize_function(
    ctx: &LowerCtx<'_>,
    program: &mut IRProgram,
    generic_fn_asts: &std::collections::HashMap<String, expo_ast::ast::Function>,
    name: &str,
    type_args: &[Type],
) -> Result<Option<FunctionIdentifier>, String> {
    let func_ast = generic_fn_asts
        .get(name)
        .ok_or_else(|| format!("no generic function `{name}` to monomorphize"))?
        .clone();

    let mangled = FunctionIdentifier::new(mangle_method_suffix(name, type_args));
    if program.contains_function(&mangled) {
        return Ok(None);
    }

    let sig = ctx
        .type_ctx
        .functions
        .get(name)
        .ok_or_else(|| format!("no signature for generic function `{name}`"))?;

    let subst = build_substitution(&sig.type_params, type_args);
    let return_type = substitute(&sig.return_type, &subst);
    let param_types: Vec<Type> = sig
        .params
        .iter()
        .map(|p| substitute(&p.ty, &subst))
        .collect();

    let meta = IRFunctionMeta::from_ast(&func_ast);
    program.insert_function(IRFunction {
        mangled: mangled.clone(),
        param_types,
        return_type,
        kind: IRFunctionKind::Free {
            func_ast,
            meta,
            subst,
            blocks: Vec::new(),
        },
    });

    Ok(Some(mangled))
}

/// Plans a monomorphized impl method: defers to
/// [`resolve_method_signature`] for the AST/types/substitutions then
/// appends an [`IRFunction`] (kind = `Method`) to `program`. Idempotent
/// against [`IRProgram::contains_function`]: returns `Ok(None)` if the
/// method is already in the program.
pub fn monomorphize_impl_method(
    ctx: &LowerCtx<'_>,
    program: &mut IRProgram,
    base_type: &str,
    method_name: &str,
    type_args: &[Type],
    method_type_args: &[Type],
) -> Result<Option<FunctionIdentifier>, String> {
    let sig = resolve_method_signature(ctx, base_type, method_name, type_args, method_type_args)?;

    if program.contains_function(&sig.mangled_fn) {
        return Ok(None);
    }

    let mangled = sig.mangled_fn.clone();
    let meta = IRFunctionMeta::from_ast(&sig.func_ast);
    program.insert_function(IRFunction {
        mangled: sig.mangled_fn,
        param_types: sig.param_types,
        return_type: sig.return_type,
        kind: IRFunctionKind::Method {
            func_ast: sig.func_ast,
            meta,
            subst: sig.subst,
            base_type: base_type.to_string(),
            mangled_type: sig.mangled_type,
            self_type: sig.self_type,
            is_static: sig.is_static,
            blocks: Vec::new(),
        },
    });

    Ok(Some(mangled))
}

/// Convenience wrapper used by `ensure_types_exist`-style call sites
/// that already have a [`TypeIdentifier`] and want to dispatch on
/// struct vs enum without re-querying `TypeContext`.
pub fn monomorphize_named(
    ctx: &LowerCtx<'_>,
    program: &mut IRProgram,
    id: &TypeIdentifier,
    type_args: &[Type],
) -> Result<(), String> {
    if ctx.type_ctx.is_enum(id.name.as_str()) {
        monomorphize_enum(ctx, program, id, type_args)?;
    } else {
        monomorphize_struct(ctx, program, id, type_args)?;
    }
    Ok(())
}

/// Resolves a single name in the current package and dispatches to
/// `monomorphize_named`. Mirrors the inner branch of `ensure_types_exist`
/// for unqualified identifier monomorphization.
pub fn monomorphize_resolved(
    ctx: &LowerCtx<'_>,
    program: &mut IRProgram,
    base: &str,
    type_args: &[Type],
) -> Result<(), String> {
    let Some(base_id) = resolve_name_current(ctx, base).cloned() else {
        return Ok(());
    };
    monomorphize_named(ctx, program, &base_id, type_args)
}
