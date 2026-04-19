//! Lowering for method-call signature resolution.
//!
//! Threads the call-site type arguments through `Self`, computes the
//! method's mangled symbol, and reports the resolved parameter / return
//! types so emission can build the LLVM call directly.

use std::collections::HashMap;

use expo_ast::ast::{ImplMember, TypeExpr, TypeParam};
use expo_typecheck::context::FunctionKind;
use expo_typecheck::types::{
    Type, build_substitution, mangle_method_suffix, mangle_name, named_generic, substitute,
};

use crate::lower::LowerCtx;
use crate::lower::types::{find_type_current, resolve_name_current};
use crate::resolved::methods::ResolvedMethodSignature;

/// Resolves the method signature for a generic impl method by looking up
/// the AST (specialized or generic path), building type substitutions,
/// and computing parameter / return types. No LLVM emission.
///
/// Returns `None` if the method has already been compiled, as reported by
/// `is_compiled` (which lets callers consult their own per-backend
/// function cache without coupling this helper to LLVM).
pub fn resolve_method_signature(
    ctx: &LowerCtx<'_>,
    base_type: &str,
    method_name: &str,
    type_args: &[Type],
    method_type_args: &[Type],
    is_compiled: impl Fn(&str) -> bool,
) -> Result<Option<ResolvedMethodSignature>, String> {
    let base_id = resolve_name_current(ctx, base_type)
        .cloned()
        .ok_or_else(|| format!("cannot resolve package for generic method base `{base_type}`"))?;
    let mangled_type = mangle_name(&base_id, type_args);
    let mangled_fn = if method_type_args.is_empty() {
        format!("{}_{}", mangled_type, method_name)
    } else {
        let mangled_method = mangle_method_suffix(method_name, method_type_args);
        format!("{}_{}", mangled_type, mangled_method)
    };
    if is_compiled(&mangled_fn) {
        return Ok(None);
    }

    let spec_id = resolve_name_current(ctx, base_type).cloned();
    let specialized_match = spec_id.as_ref().and_then(|id| {
        ctx.type_ctx
            .specialized_impl_asts
            .get(id)
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|(concrete_args, _)| concrete_args == type_args)
                    .cloned()
            })
    });

    let (func_ast, subst, return_type, param_types, is_static) =
        if let Some((concrete_args, spec_block)) = specialized_match {
            let mut method_ast = None;
            for member in &spec_block.members {
                if let ImplMember::Function(f) = member
                    && f.name == method_name
                {
                    method_ast = Some(f.clone());
                    break;
                }
            }
            let func_ast = method_ast.ok_or_else(|| {
                format!("method `{method_name}` not found in specialized impl for `{base_type}`")
            })?;

            let mut subst = HashMap::new();
            for (tp, ta) in func_ast.type_params.iter().zip(method_type_args.iter()) {
                subst.insert(tp.name.clone(), ta.clone());
            }

            let spec_sig = spec_id
            .as_ref()
            .and_then(|id| {
                ctx.type_ctx
                    .specialized_methods
                    .get(id)
                    .and_then(|entries| {
                        entries
                            .iter()
                            .find(|(args, _)| *args == concrete_args)
                            .and_then(|(_, sigs)| sigs.get(method_name))
                    })
            })
            .ok_or_else(|| {
                format!(
                    "no signature for method `{method_name}` in specialized impl for `{base_type}`"
                )
            })?;

            let ret = substitute(&spec_sig.return_type, &subst);
            let pts: Vec<Type> = spec_sig
                .params
                .iter()
                .map(|p| substitute(&p.ty, &subst))
                .collect();
            let is_static = spec_sig.kind == FunctionKind::Static;
            (func_ast, subst, ret, pts, is_static)
        } else {
            let impl_blocks = ctx
                .type_ctx
                .generic_impl_asts
                .get(base_type)
                .ok_or_else(|| format!("no generic impl for `{base_type}`"))?
                .clone();

            let mut method_ast = None;
            let mut impl_type_params: Vec<TypeParam> = Vec::new();
            for block in &impl_blocks {
                if let TypeExpr::Generic { args, .. } = &block.target {
                    let impl_tps: Vec<TypeParam> = args
                        .iter()
                        .filter_map(|a| {
                            if let TypeExpr::Named { path, span, .. } = a
                                && path.len() == 1
                            {
                                return Some(TypeParam {
                                    name: path[0].clone(),
                                    bounds: Vec::new(),
                                    span: *span,
                                });
                            }
                            None
                        })
                        .collect();
                    for member in &block.members {
                        if let ImplMember::Function(f) = member
                            && f.name == method_name
                        {
                            method_ast = Some(f.clone());
                            impl_type_params = impl_tps;
                            break;
                        }
                    }
                    if method_ast.is_some() {
                        break;
                    }
                }
            }

            let func_ast = method_ast.ok_or_else(|| {
                format!("method `{method_name}` not found in impl for `{base_type}`")
            })?;

            let mut subst = build_substitution(&impl_type_params, type_args);
            for (tp, ta) in func_ast.type_params.iter().zip(method_type_args.iter()) {
                subst.insert(tp.name.clone(), ta.clone());
            }

            let info = find_type_current(ctx, base_type).map(|ti| (&ti.functions, &ti.type_params));

            let (return_type, param_types, is_static) = if let Some((methods, _)) = info {
                if let Some(sig) = methods.get(method_name) {
                    let ret = substitute(&sig.return_type, &subst);
                    let pts: Vec<Type> = sig
                        .params
                        .iter()
                        .map(|p| substitute(&p.ty, &subst))
                        .collect();
                    let is_static = sig.kind == FunctionKind::Static;
                    (ret, pts, is_static)
                } else {
                    return Err(format!(
                        "no signature for method `{method_name}` on `{base_type}`"
                    ));
                }
            } else {
                return Err(format!("no type info for `{base_type}`"));
            };
            (func_ast, subst, return_type, param_types, is_static)
        };

    let self_type = if is_static {
        None
    } else if base_type == "CPtr" {
        Some(Type::Pointer(Box::new(
            type_args.first().cloned().unwrap_or(Type::Unknown),
        )))
    } else {
        Some(named_generic(
            base_type,
            type_args.to_vec(),
            ctx.type_ctx,
            ctx.package,
        ))
    };

    Ok(Some(ResolvedMethodSignature {
        func_ast,
        is_static,
        mangled_fn,
        mangled_type,
        param_types,
        return_type,
        self_type,
        subst,
    }))
}
