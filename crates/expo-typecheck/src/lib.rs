mod check;
mod collect;
pub mod context;
mod cycle;
mod env;
mod expr;
mod pattern;
mod stmt;
pub mod types;

use std::collections::HashMap;

use context::{FunctionSig, ParamInfo, TypeContext};
use expo_ast::ast::{Module, Param};
use types::resolve_type_expr_with_params;

/// The source of `std.kernel`, embedded at compile time. Callers parse this
/// with `expo_parser::parse` and pass the resulting context to [`merge_stdlib`].
pub const KERNEL_SOURCE: &str = include_str!("../std/kernel.expo");

/// The source of `std.bitwise`, embedded at compile time. Provides the
/// `Bitwise` protocol and intrinsic implementations for all integer types.
pub const BITWISE_SOURCE: &str = include_str!("../std/bitwise.expo");

/// All embedded stdlib sources in dependency order. Kernel must come first;
/// subsequent modules may reference types defined by earlier ones.
pub const STDLIB_SOURCES: &[&str] = &[KERNEL_SOURCE, BITWISE_SOURCE];

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
    for (name, ast) in &stdlib.generic_protocol_asts {
        if !target.generic_protocol_asts.contains_key(name) {
            target
                .generic_protocol_asts
                .insert(name.clone(), ast.clone());
        }
    }
    for (name, info) in &stdlib.protocols {
        if !target.protocols.contains_key(name) {
            target.protocols.insert(name.clone(), info.clone());
        }
    }
    for (type_name, protos) in &stdlib.protocol_impls {
        target
            .protocol_impls
            .entry(type_name.clone())
            .or_default()
            .extend(protos.iter().cloned());
    }
    for (prim_name, methods) in &stdlib.primitive_methods {
        let entry = target
            .primitive_methods
            .entry(prim_name.clone())
            .or_default();
        for (method_name, sig) in methods {
            if !entry.contains_key(method_name) {
                entry.insert(method_name.clone(), sig.clone());
            }
        }
    }
    for (span, captures) in &stdlib.closure_captures {
        target.closure_captures.insert(*span, captures.clone());
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

/// Synthesizes default protocol methods for impls whose protocols were unknown
/// during initial collection (e.g. after merging stdlib). Must be called after
/// [`merge_stdlib`].
pub fn synthesize_protocol_defaults(module: &Module, ctx: &mut TypeContext) {
    collect::synthesize_protocol_defaults(module, ctx);
}

/// Detects recursive struct/enum fields and wraps them in [`types::Type::Indirect`]
/// for heap-allocated indirection. Must be called after [`re_resolve_generics`].
pub fn mark_recursive_fields(ctx: &mut TypeContext) {
    cycle::mark_recursive_fields(ctx);
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

    let type_aliases = ctx.type_aliases.clone();

    let generic_struct_names: Vec<String> = ctx.generic_struct_asts.keys().cloned().collect();
    for name in &generic_struct_names {
        let decl = ctx.generic_struct_asts[name].clone();
        let tp_refs: Vec<&str> = decl.type_params.iter().map(|s| s.as_str()).collect();

        let fields: Vec<(String, types::Type)> = decl
            .fields
            .iter()
            .map(|f| {
                let ty = resolve_type_expr_with_params(
                    &f.type_expr,
                    &struct_refs,
                    &enum_refs,
                    &tp_refs,
                    &type_aliases,
                );
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
                    mode,
                    name,
                    type_expr,
                    ..
                } => Some(ParamInfo {
                    mode: *mode,
                    name: name.clone(),
                    ty: resolve_type_expr_with_params(
                        type_expr,
                        &struct_refs,
                        &enum_refs,
                        &tp_refs,
                        &type_aliases,
                    ),
                }),
                Param::Self_ { .. } => None,
            })
            .collect();

        let return_type = func
            .return_type
            .as_ref()
            .map(|t| {
                resolve_type_expr_with_params(t, &struct_refs, &enum_refs, &tp_refs, &type_aliases)
            })
            .unwrap_or(types::Type::Unit);

        if let Some(sig) = ctx.functions.get_mut(&name.to_string()) {
            *sig = FunctionSig {
                visibility: sig.visibility,
                params,
                return_type,
                kind: sig.kind,
                span: sig.span,
                type_params: sig.type_params.clone(),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_source(src: &str) -> TypeContext {
        let parse_result = expo_parser::parse(src);
        check(&parse_result.module)
    }

    fn errors(ctx: &TypeContext) -> Vec<&str> {
        ctx.diagnostics.iter().map(|d| d.message.as_str()).collect()
    }

    #[test]
    fn binary_literal_infers_binary_type() {
        let ctx = check_source("fn main\n  x = <<0xFF, 0x00>>\nend\n");
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn binary_literal_non_byte_aligned_infers_bits() {
        let ctx = check_source("fn main\n  x = <<1::3, 0::5>>\nend\n");
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn binary_literal_empty_infers_binary() {
        let ctx = check_source("fn main\n  x = <<>>\nend\n");
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn binary_literal_overflow_detected() {
        let ctx = check_source("fn main\n  x = <<256>>\nend\n");
        assert!(
            errors(&ctx).iter().any(|e| e.contains("does not fit")),
            "expected overflow error, got: {:?}",
            errors(&ctx)
        );
    }

    #[test]
    fn binary_pattern_binds_int_for_sized_segment() {
        let ctx = check_source(
            "fn main\n  data: Binary = <<>>\n  match data\n    <<tag::8, _rest: Binary>> -> tag\n    _ -> 0\n  end\nend\n",
        );
        assert!(errors(&ctx).is_empty(), "errors: {:?}", errors(&ctx));
    }

    #[test]
    fn binary_pattern_requires_catch_all() {
        let ctx = check_source(
            "fn main\n  data: Binary = <<>>\n  match data\n    <<tag::8>> -> tag\n  end\nend\n",
        );
        assert!(
            errors(&ctx).iter().any(|e| e.contains("catch-all")),
            "expected catch-all error, got: {:?}",
            errors(&ctx)
        );
    }

    #[test]
    fn binary_pattern_rejects_non_binary_subject() {
        let ctx = check_source(
            "fn main\n  x = 42\n  match x\n    <<tag::8>> -> tag\n    _ -> 0\n  end\nend\n",
        );
        assert!(
            errors(&ctx)
                .iter()
                .any(|e| e.contains("Binary") || e.contains("Bits")),
            "expected binary subject error, got: {:?}",
            errors(&ctx)
        );
    }

    #[test]
    fn binary_pattern_greedy_rest_must_be_last() {
        let ctx = check_source(
            "fn main\n  data: Binary = <<>>\n  match data\n    <<rest: Binary, tag::8>> -> tag\n    _ -> 0\n  end\nend\n",
        );
        assert!(
            errors(&ctx).iter().any(|e| e.contains("last segment")),
            "expected greedy-rest error, got: {:?}",
            errors(&ctx)
        );
    }
}
