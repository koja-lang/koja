mod check;
mod collect;
pub mod context;
pub mod types;

use std::collections::HashMap;

use context::{FunctionSig, ParamInfo, TypeContext};
use expo_ast::ast::{Module, Param};
use types::resolve_type_expr_with_params;

/// The source of `std.kernel`, embedded at compile time. Callers parse this
/// with `expo_parser::parse` and pass the resulting context to [`merge_stdlib`].
pub const KERNEL_SOURCE: &str = include_str!("../std/kernel.expo");

/// Merges a stdlib [`TypeContext`] into `target`, adding any types, functions,
/// and generic ASTs that aren't already defined in the target module.
pub fn merge_stdlib(stdlib: &TypeContext, target: &mut TypeContext) {
    for (name, info) in &stdlib.structs {
        if !target.structs.contains_key(name) {
            target.structs.insert(name.clone(), info.clone());
        }
    }
    for (name, info) in &stdlib.enums {
        if !target.enums.contains_key(name) {
            target.enums.insert(name.clone(), info.clone());
        }
    }
    for (name, sig) in &stdlib.functions {
        if !target.functions.contains_key(name) {
            target.functions.insert(name.clone(), sig.clone());
        }
    }
    for (name, ast) in &stdlib.generic_struct_asts {
        if !target.generic_struct_asts.contains_key(name) {
            target.generic_struct_asts.insert(name.clone(), ast.clone());
        }
    }
    for (name, ast) in &stdlib.generic_enum_asts {
        if !target.generic_enum_asts.contains_key(name) {
            target.generic_enum_asts.insert(name.clone(), ast.clone());
        }
    }
    for (name, blocks) in &stdlib.generic_impl_asts {
        target
            .generic_impl_asts
            .entry(name.clone())
            .or_default()
            .extend(blocks.iter().cloned());
    }
}

/// Runs collection and type-checking in one step, returning a populated context.
pub fn check(module: &Module) -> TypeContext {
    let mut ctx = collect::collect(module);
    check::check_module(module, &mut ctx);
    ctx
}

/// Validates all function bodies, expressions, and patterns against the context.
pub fn check_module(module: &Module, ctx: &mut TypeContext) {
    check::check_module(module, ctx);
}

/// Walks the AST to collect type signatures for functions, structs, and enums.
pub fn collect_module(module: &Module) -> TypeContext {
    collect::collect(module)
}

/// Merges imported module contexts into the current context based on import statements.
pub fn resolve_imports(
    module: &Module,
    ctx: &mut TypeContext,
    module_contexts: &HashMap<String, TypeContext>,
) {
    collect::resolve_imports(module, ctx, module_contexts);
}

/// Re-resolves generic type signatures that may have `Type::Unknown` fields,
/// parameters, or return types because the referenced types (e.g. stdlib types)
/// weren't known during initial collection. Must be called after merging stdlib.
pub fn re_resolve_generics(ctx: &mut TypeContext) {
    let struct_names: Vec<String> = ctx.structs.keys().cloned().collect();
    let enum_names: Vec<String> = ctx.enums.keys().cloned().collect();
    let struct_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();
    let enum_refs: Vec<&str> = enum_names.iter().map(|s| s.as_str()).collect();

    let generic_struct_names: Vec<String> = ctx.generic_struct_asts.keys().cloned().collect();
    for name in &generic_struct_names {
        let decl = ctx.generic_struct_asts[name].clone();
        let tp_refs: Vec<&str> = decl.type_params.iter().map(|s| s.as_str()).collect();

        let fields: Vec<(String, types::Type)> = decl
            .fields
            .iter()
            .map(|f| {
                let ty =
                    resolve_type_expr_with_params(&f.type_expr, &struct_refs, &enum_refs, &tp_refs);
                (f.name.clone(), ty)
            })
            .collect();

        if let Some(info) = ctx.structs.get_mut(name) {
            info.fields = fields;
        }
    }

    let fn_names: Vec<String> = ctx.generic_function_asts.keys().cloned().collect();
    for name in &fn_names {
        let func = ctx.generic_function_asts[name].clone();
        let tp_refs: Vec<&str> = func.type_params.iter().map(|s| s.as_str()).collect();

        let params: Vec<ParamInfo> = func
            .params
            .iter()
            .filter_map(|p| match p {
                Param::Regular {
                    name, type_expr, ..
                } => Some(ParamInfo {
                    name: name.clone(),
                    ty: resolve_type_expr_with_params(
                        type_expr,
                        &struct_refs,
                        &enum_refs,
                        &tp_refs,
                    ),
                }),
                Param::Self_ { .. } => None,
            })
            .collect();

        let return_type = func
            .return_type
            .as_ref()
            .map(|t| resolve_type_expr_with_params(t, &struct_refs, &enum_refs, &tp_refs))
            .unwrap_or(types::Type::Unit);

        if let Some(sig) = ctx.functions.get_mut(&name.to_string()) {
            *sig = FunctionSig {
                is_private: sig.is_private,
                params,
                return_type,
                span: sig.span,
                type_params: sig.type_params.clone(),
            };
        }
    }
}
