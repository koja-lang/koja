//! Lowering for bare-name function calls.
//!
//! Decides which `ResolvedCall` variant a call expression resolves to:
//! struct constructor, builtin (`panic`/`print`), direct call to a
//! defined symbol, indirect call through a closure-typed variable, or
//! generic that needs monomorphization. Mangled-symbol selection
//! (package-qualifying user methods, leaving stdlib symbols bare) and
//! signature lookup happen here. Callable-symbol existence is queried
//! through `program.contains_function(...)` on [`IRProgram`] (the
//! canonical registry); the remaining `impl Fn(...)` parameters bridge
//! to LLVM-bound caches in `expo-codegen` (`fn_state.variables`,
//! `llvm_types`, `generic_fn_asts`) without coupling `expo-ir` to a
//! backend. Emission uses the chosen mangled name (and the variable
//! name from the call site) to fetch the actual
//! `FunctionValue`/`PointerValue` post-dispatch.

use expo_ast::ast::{Arg, TypeParam};
use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::types::{Type, build_substitution, mangle_name, substitute, unwrap_indirect};

use crate::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier};
use crate::lower::ctx::LowerCtx;
use crate::lower::inference::infer_static_struct_type_args_from_args;
use crate::lower::naming::{current_method_symbol_prefix, method_symbol_prefix};
use crate::lower::types::{id_for, resolve_name_current};
use crate::program::IRProgram;
use crate::resolved::calls::{
    BuiltinCall, PendingMethodMono, PendingTypeMono, ResolvedCall, ResolvedStaticCall,
};

/// Resolves a bare-name function call to a [`ResolvedCall`].
///
/// Callable-symbol existence reads from `program.contains_function`;
/// the remaining closures bridge to LLVM-bound caches that live on
/// the codegen `Compiler` (struct-constructor type cache, local
/// variables, generic AST cache); each is consulted at most twice.
pub fn resolve_call(
    ctx: &LowerCtx<'_>,
    program: &IRProgram,
    name: &str,
    is_struct_constructor: impl Fn(Option<&TypeIdentifier>, &str) -> bool,
    variable_type: impl Fn(&str) -> Option<Type>,
    is_generic_function: impl Fn(&str) -> bool,
) -> Result<ResolvedCall, String> {
    let resolved_id = resolve_name_current(ctx, name).cloned();

    if is_struct_constructor(resolved_id.as_ref(), name) {
        return Ok(ResolvedCall::StructConstructor {
            identifier: resolved_id,
        });
    }

    match name {
        "panic" => return Ok(ResolvedCall::Builtin(BuiltinCall::Panic)),
        "print" | "print_Bool" | "print_Float" | "print_Int" | "print_Int32" | "print_String" => {
            return Ok(ResolvedCall::Builtin(BuiltinCall::Print));
        }
        _ => {}
    }

    // When we're inside a method body, the unqualified call `foo(..)` can also
    // refer to another method on the same type. Build the candidate LLVM symbol
    // using the same package-qualifying rule as definition-site mangling so the
    // lookup succeeds for user packages (e.g. `crypto.HMAC_hmac_raw`) without
    // breaking stdlib symbols (e.g. `Int_hash`).
    let mangled_candidate = ctx.fn_lower.self_type_name.as_ref().map(|type_name| {
        let prefix = current_method_symbol_prefix(ctx, type_name);
        format!("{prefix}_{name}")
    });

    let chosen_mangled: Option<FunctionIdentifier> =
        if program.contains_function(&FunctionIdentifier::new(name)) {
            Some(FunctionIdentifier::new(name))
        } else {
            mangled_candidate
                .as_ref()
                .map(FunctionIdentifier::new)
                .filter(|candidate| program.contains_function(candidate))
        };

    if let Some(mangled_name) = chosen_mangled {
        let signature = ctx.type_ctx.function_sig(name).or_else(|| {
            ctx.fn_lower
                .self_type_name
                .as_ref()
                .and_then(|type_name| resolve_name_current(ctx, type_name))
                .and_then(|id| ctx.type_ctx.get_type(id))
                .and_then(|type_info| type_info.functions.get(name))
        });
        let param_types: Vec<Type> = signature
            .map(|sig| sig.params.iter().map(|param| param.ty.clone()).collect())
            .unwrap_or_default();
        let return_type = signature
            .map(|sig| sig.return_type.clone())
            .unwrap_or(Type::Unknown);
        return Ok(ResolvedCall::Direct {
            mangled_name,
            param_types,
            return_type,
        });
    }

    if let Some(raw_type) = variable_type(name) {
        let inner = unwrap_indirect(&raw_type);
        let Type::Function {
            params,
            return_type,
        } = inner.clone()
        else {
            return Err(format!("undefined function: {name}"));
        };
        return Ok(ResolvedCall::ClosureVariable {
            params,
            return_type: *return_type,
        });
    }

    if is_generic_function(name) {
        return Ok(ResolvedCall::Generic);
    }

    Err(format!("undefined function: {name}"))
}

/// Resolves the call target for `Type.method(args)` (a static method
/// call): chooses the mangled callee symbol, computes the parameter /
/// return types, and reports any monomorphization the caller must
/// trigger before looking up the LLVM `FunctionValue`.
///
/// Generic static calls thread two side-conditions back to the caller:
/// 1. `pending_type_mono` — the receiver type itself may not be
///    monomorphized yet (e.g. `List<Int>.new()` requires `List<Int>`'s
///    LLVM struct to exist before the static call's signature is built).
/// 2. `pending_mono` — the static method's mangled symbol may not be
///    emitted; the caller calls `monomorphize_impl_method` (which
///    handles stdlib intrinsic dispatch + IR planning + LLVM emission).
///
/// `infer_arg_type` is the same closure pattern as `var_type` for
/// methods: it bridges to `Compiler.fn_state.variables` for argument
/// type inference of static calls whose type-args must be inferred
/// from arguments (e.g. `Task.async(f)`).
#[allow(clippy::too_many_arguments)]
pub fn resolve_static_call(
    ctx: &LowerCtx<'_>,
    program: &IRProgram,
    var_type: &dyn Fn(&str) -> Option<Type>,
    type_mono_exists: &dyn Fn(&MonomorphizedTypeIdentifier) -> bool,
    type_name: &str,
    resolved_type: Option<&TypeIdentifier>,
    method: &str,
    args: &[Arg],
) -> Result<ResolvedStaticCall, String> {
    let resolved_id = id_for(ctx, type_name, resolved_type);
    let type_params: Option<Vec<TypeParam>> = resolved_id
        .as_ref()
        .and_then(|id| ctx.type_ctx.get_type(id))
        .map(|ti| ti.type_params.clone());

    let mut type_args: Vec<Type> = if let Some(ref tp) = type_params
        && !tp.is_empty()
    {
        tp.iter()
            .filter_map(|param| ctx.fn_lower.type_subst.get(&param.name).cloned())
            .collect()
    } else {
        Vec::new()
    };

    if let Some(ref tp) = type_params
        && !tp.is_empty()
        && type_args.len() != tp.len()
    {
        type_args =
            infer_static_struct_type_args_from_args(ctx, var_type, type_name, method, args, tp)?;
    }

    let mut pending_type_mono: Option<PendingTypeMono> = None;
    let mangled_type = if type_args.is_empty() {
        type_name.to_string()
    } else {
        let type_id = resolved_id.clone().ok_or_else(|| {
            format!("cannot resolve package for generic static call on `{type_name}`")
        })?;
        let m = mangle_name(&type_id, &type_args);
        if !type_mono_exists(&MonomorphizedTypeIdentifier::new(&m)) {
            pending_type_mono = Some(PendingTypeMono {
                identifier: type_id,
                type_args: type_args.clone(),
                is_enum: ctx.type_ctx.is_enum(type_name),
            });
        }
        m
    };

    // Pick the symbol prefix in lockstep with definition-site mangling:
    // non-generic user types use `{pkg}.{TypeName}`; stdlib/primitives and
    // generics keep the existing bare-name prefix until later migration stages.
    let symbol_prefix = if type_args.is_empty() {
        resolved_id
            .as_ref()
            .map(|id| method_symbol_prefix(&id.package, &id.name))
            .unwrap_or_else(|| mangled_type.clone())
    } else {
        mangled_type.clone()
    };

    let mangled_name = format!("{symbol_prefix}_{method}");

    let mut pending_mono: Option<PendingMethodMono> = None;
    if !program.contains_function(&FunctionIdentifier::new(&mangled_name)) {
        if !type_args.is_empty() {
            pending_mono = Some(PendingMethodMono {
                base_type: type_name.to_string(),
                method: method.to_string(),
                type_args: type_args.clone(),
                method_type_args: Vec::new(),
            });
        } else {
            return Err(format!(
                "undefined static function `{method}` on `{type_name}`"
            ));
        }
    }

    let (param_types, return_type) = ctx
        .type_ctx
        .functions
        .get(&mangled_name)
        .map(|sig| {
            let pts: Vec<Type> = sig.params.iter().map(|p| p.ty.clone()).collect();
            (pts, sig.return_type.clone())
        })
        .or_else(|| {
            let ti = resolved_id
                .as_ref()
                .and_then(|id| ctx.type_ctx.get_type(id))?;
            let sig = ti.functions.get(method)?;
            if !type_args.is_empty() {
                let subst = build_substitution(&ti.type_params, &type_args);
                let pts = sig
                    .params
                    .iter()
                    .map(|p| substitute(&p.ty, &subst))
                    .collect();
                Some((pts, substitute(&sig.return_type, &subst)))
            } else {
                let pts = sig.params.iter().map(|p| p.ty.clone()).collect();
                Some((pts, sig.return_type.clone()))
            }
        })
        .unwrap_or_else(|| (Vec::new(), Type::Unknown));

    Ok(ResolvedStaticCall {
        mangled_name: FunctionIdentifier::new(mangled_name),
        param_types,
        return_type,
        pending_type_mono,
        pending_mono,
    })
}
