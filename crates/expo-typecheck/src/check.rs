//! Module and function-level type checking entry points.
//!
//! Contains [`check_module`], the public entry point that walks all function
//! bodies and impl blocks, plus shared helper functions used across the
//! type-checking modules.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use expo_ast::ast::*;
use expo_ast::span::Span;

use crate::context::{Coercion, FunctionKind, FunctionSig, ParamInfo, PassMode, TypeContext};
use crate::env::{CheckEnv, VarInfo, VarState};
use crate::expr::{expr_span, infer_expr, infer_expr_with_expected};
use crate::stmt::check_body;
use crate::types::numeric_compatible;
use crate::types::{
    Package, Primitive, Type, TypeIdentifier, named, package_from_str,
    resolve_type_expr_with_params,
};

/// Classifies an `impl` target into the form used by body type-checking.
///
/// Returns `Some((target_name, impl_type_params))` when the target is one of:
/// - `impl Foo` or `impl Trait for Foo` -> `(Foo, [])`
/// - `impl Foo<T, U>` or `impl Trait<...> for Foo<T, U>` -> `(Foo, [T, U])`
///
/// Returns `None` for unsupported shapes (multi-segment paths, weird args)
/// or for cases handled elsewhere:
/// - Specialized impls like `impl Foo<Int>` -- see [`classify_specialized_impl_target`].
/// - Mixed concrete + type parameter args have already been reported as
///   errors during collection.
fn classify_impl_target(
    target: &TypeExpr,
    struct_names: &[&str],
    enum_names: &[&str],
) -> Option<(String, Vec<TypeParam>)> {
    match target {
        TypeExpr::Named { path, .. } if path.len() == 1 => Some((path[0].clone(), Vec::new())),
        TypeExpr::Generic { path, args, .. } if path.len() == 1 => {
            let mut type_params = Vec::new();
            let mut concrete_count = 0;
            for arg in args {
                let TypeExpr::Named { path: p, span } = arg else {
                    continue;
                };
                if p.len() != 1 {
                    continue;
                }
                let name = &p[0];
                if Primitive::from_name(name).is_some()
                    || struct_names.contains(&name.as_str())
                    || enum_names.contains(&name.as_str())
                {
                    concrete_count += 1;
                } else {
                    type_params.push(TypeParam {
                        name: name.clone(),
                        bounds: Vec::new(),
                        span: *span,
                    });
                }
            }
            if concrete_count > 0 {
                // Specialized (`impl Foo<Int>`) or mixed -- handled elsewhere.
                return None;
            }
            Some((path[0].clone(), type_params))
        }
        _ => None,
    }
}

/// Classifies a specialized `impl` target (`impl Foo<Int>` /
/// `impl Trait<...> for Foo<Int>`) into `(target_name, concrete_args)`.
///
/// Returns `None` when the target isn't fully specialized -- generic and
/// plain impls flow through [`classify_impl_target`] instead.
fn classify_specialized_impl_target(
    target: &TypeExpr,
    struct_names: &[&str],
    enum_names: &[&str],
) -> Option<(String, Vec<Type>)> {
    let TypeExpr::Generic { path, args, .. } = target else {
        return None;
    };
    if path.len() != 1 || args.is_empty() {
        return None;
    }
    let mut concrete_args = Vec::with_capacity(args.len());
    for arg in args {
        let TypeExpr::Named { path: p, .. } = arg else {
            return None;
        };
        if p.len() != 1 {
            return None;
        }
        let name = &p[0];
        if let Some(prim) = Primitive::from_name(name) {
            concrete_args.push(Type::Primitive(prim));
        } else if struct_names.contains(&name.as_str()) || enum_names.contains(&name.as_str()) {
            concrete_args.push(named(name));
        } else {
            // Free type parameter -- not a specialized impl.
            return None;
        }
    }
    Some((path[0].clone(), concrete_args))
}

/// Type-checks all function bodies and impl blocks in a module, emitting
/// diagnostics for type mismatches, undefined variables, and exhaustiveness errors.
///
/// `package` identifies which package the module belongs to (e.g. `"std"`,
/// `"alpha"`, or a synthetic name like `"__test__"` derived from the file
/// stem). It is installed as the context's ambient scope so that bare-name
/// type lookups prefer the module's own package over colliding definitions
/// in other packages.
pub fn check_module(module: &mut Module, ctx: &mut TypeContext, package: &str) {
    let prev_path = ctx.current_module_path.clone();
    ctx.current_module_path = module.path.clone();
    let prev_package = ctx.current_package.clone();
    ctx.current_package = Some(package_from_str(package));

    let struct_names = ctx.struct_names();
    let struct_name_refs: Vec<&str> = struct_names.iter().map(|s| s.as_str()).collect();
    let enum_names = ctx.enum_names();
    let enum_name_refs: Vec<&str> = enum_names.iter().map(|s| s.as_str()).collect();

    for item in &mut module.items {
        match item {
            Item::Function(f) => {
                if f.type_params.is_empty() {
                    check_function(f, ctx, None, &struct_name_refs, &enum_name_refs, None);
                }
            }
            Item::Struct(s) if !s.type_params.is_empty() => {
                // Generic -- checked during monomorphization
            }
            Item::Struct(s) => {
                let mut self_type = named(&s.name);
                ctx.resolve_type(&mut self_type);
                check_inline_functions(
                    &mut s.functions,
                    &s.name,
                    &self_type,
                    ctx,
                    &struct_name_refs,
                    &enum_name_refs,
                );
            }
            Item::Enum(e) if !e.type_params.is_empty() => {
                // Generic -- checked during monomorphization
            }
            Item::Enum(e) => {
                let mut self_type = named(&e.name);
                ctx.resolve_type(&mut self_type);
                check_inline_functions(
                    &mut e.functions,
                    &e.name,
                    &self_type,
                    ctx,
                    &struct_name_refs,
                    &enum_name_refs,
                );
            }
            Item::Impl(impl_block) => {
                check_impl_block(impl_block, ctx, &struct_name_refs, &enum_name_refs);
            }
            _ => {}
        }
    }

    ctx.current_package = prev_package;
    ctx.current_module_path = prev_path;
}

/// Type-checks inline functions defined inside a struct or enum body.
fn check_inline_functions(
    functions: &mut [Function],
    type_name: &str,
    self_type: &Type,
    ctx: &mut TypeContext,
    struct_names: &[&str],
    enum_names: &[&str],
) {
    let type_id = ctx.resolve_name(type_name).cloned();
    let process_msg = type_id.as_ref().and_then(|id| ctx.process_envelope_for(id));
    for f in functions {
        if f.type_params.is_empty() {
            check_function_with_msg(
                f,
                ctx,
                Some(self_type),
                struct_names,
                enum_names,
                process_msg.clone(),
                type_id.as_ref(),
                &[],
                None,
            );
        }
    }
}

/// Type-checks all method bodies in an `impl` block.
///
/// Handles non-generic impls (`impl Foo` / `impl Trait for Foo`), generic
/// impls (`impl Foo<T>` / `impl Trait<...> for Foo<T>`), and specialized
/// impls (`impl Foo<Int>` / `impl Trait<...> for Foo<Int>`).
///
/// For generic impls the impl-level type parameters are surfaced as
/// `Type::Parameter`s in `self_type` and threaded into each method's
/// `CheckEnv.fn_type_params`, so generic-receiver method dispatch and
/// `Self` substitution work the same way they do for free generic
/// functions. For specialized impls the concrete args are threaded through
/// to [`lookup_sig`] so it can find the per-specialization signatures
/// stored in `ctx.specialized_methods`.
fn check_impl_block(
    impl_block: &mut ImplBlock,
    ctx: &mut TypeContext,
    struct_names: &[&str],
    enum_names: &[&str],
) {
    if let Some((target_name, concrete_args)) =
        classify_specialized_impl_target(&impl_block.target, struct_names, enum_names)
    {
        check_specialized_impl_block(
            impl_block,
            &target_name,
            concrete_args,
            ctx,
            struct_names,
            enum_names,
        );
        return;
    }

    let Some((target_name, impl_type_params)) =
        classify_impl_target(&impl_block.target, struct_names, enum_names)
    else {
        return;
    };

    let tp_names: Vec<&str> = impl_type_params.iter().map(|tp| tp.name.as_str()).collect();
    let known_packages: BTreeSet<Package> = ctx.package_types.keys().cloned().collect();
    let mut self_type = if !impl_type_params.is_empty() {
        resolve_type_expr_with_params(
            &impl_block.target,
            struct_names,
            enum_names,
            &tp_names,
            &BTreeMap::new(),
            &known_packages,
        )
    } else if ctx.is_struct(&target_name) || ctx.is_enum(&target_name) {
        named(&target_name)
    } else if let Some(p) = Primitive::from_name(&target_name) {
        Type::Primitive(p)
    } else {
        return;
    };
    ctx.resolve_type(&mut self_type);

    let type_id = ctx.resolve_name(&target_name).cloned();
    let impl_process_msg = type_id.as_ref().and_then(|id| ctx.process_envelope_for(id));

    for member in &mut impl_block.members {
        if let ImplMember::Function(f) = member {
            check_function_with_msg(
                f,
                ctx,
                Some(&self_type),
                struct_names,
                enum_names,
                impl_process_msg.clone(),
                type_id.as_ref(),
                &impl_type_params,
                None,
            );
        }
    }
    let mut synth_fns = ctx
        .synthesized_default_fns
        .get(target_name.as_str())
        .cloned()
        .unwrap_or_default();
    for f in &mut synth_fns {
        check_function_with_msg(
            f,
            ctx,
            Some(&self_type),
            struct_names,
            enum_names,
            impl_process_msg.clone(),
            type_id.as_ref(),
            &impl_type_params,
            None,
        );
    }
}

/// Type-checks the methods of a specialized `impl` block (`impl Foo<Int>`).
///
/// `concrete_args` are the resolved type arguments (e.g. `[Type::Primitive(Int)]`).
/// They are passed to [`lookup_sig`] so the per-specialization signature
/// stored in `ctx.specialized_methods` can be found and used to seed the
/// method's `CheckEnv`.
fn check_specialized_impl_block(
    impl_block: &mut ImplBlock,
    target_name: &str,
    mut concrete_args: Vec<Type>,
    ctx: &mut TypeContext,
    struct_names: &[&str],
    enum_names: &[&str],
) {
    for ty in &mut concrete_args {
        ctx.resolve_type(ty);
    }

    let known_packages: BTreeSet<Package> = ctx.package_types.keys().cloned().collect();
    let mut self_type = resolve_type_expr_with_params(
        &impl_block.target,
        struct_names,
        enum_names,
        &[],
        &BTreeMap::new(),
        &known_packages,
    );
    ctx.resolve_type(&mut self_type);

    let type_id = ctx.resolve_name(target_name).cloned();
    let impl_process_msg = type_id.as_ref().and_then(|id| ctx.process_envelope_for(id));

    for member in &mut impl_block.members {
        if let ImplMember::Function(f) = member {
            check_function_with_msg(
                f,
                ctx,
                Some(&self_type),
                struct_names,
                enum_names,
                impl_process_msg.clone(),
                type_id.as_ref(),
                &[],
                Some(&concrete_args),
            );
        }
    }
}

/// Type-checks a single function body using the default message type.
fn check_function(
    f: &mut Function,
    ctx: &mut TypeContext,
    self_type: Option<&Type>,
    struct_names: &[&str],
    enum_names: &[&str],
    enclosing_type: Option<&TypeIdentifier>,
) {
    check_function_with_msg(
        f,
        ctx,
        self_type,
        struct_names,
        enum_names,
        None,
        enclosing_type,
        &[],
        None,
    );
}

/// Looks up the already-collected [`FunctionSig`] for a function. Methods are
/// found via the enclosing type's `TypeInfo`; module-level functions live in
/// `ctx.functions`. When `specialized_args` is provided, the per-
/// specialization signatures in `ctx.specialized_methods` are consulted
/// first so `impl Foo<Int>` methods resolve to their concrete sigs rather
/// than the generic ones.
fn lookup_sig<'a>(
    name: &str,
    ctx: &'a TypeContext,
    enclosing_type: Option<&TypeIdentifier>,
    specialized_args: Option<&[Type]>,
) -> Option<&'a FunctionSig> {
    if let (Some(tid), Some(args)) = (enclosing_type, specialized_args)
        && let Some(entries) = ctx.specialized_methods.get(tid)
    {
        for (concrete, sigs) in entries {
            if concrete.as_slice() == args
                && let Some(sig) = sigs.get(name)
            {
                return Some(sig);
            }
        }
    }
    if let Some(tid) = enclosing_type {
        ctx.get_type(tid).and_then(|ti| ti.functions.get(name))
    } else {
        ctx.functions.get(name)
    }
}

/// Type-checks a function body, building a [`CheckEnv`] from its parameters
/// and verifying the return type against the declared signature. When
/// `override_msg_type` is `Some`, it replaces the process mailbox type.
///
/// `impl_type_params` carries the type parameters introduced by the enclosing
/// generic `impl` block (empty for non-impl callers and non-generic impls).
/// They are merged with the method's own `type_params` into
/// `CheckEnv.fn_type_params` so generic-receiver dispatch and `Self`
/// substitution have access to both layers of generics.
#[allow(clippy::too_many_arguments)]
fn check_function_with_msg(
    f: &mut Function,
    ctx: &mut TypeContext,
    self_type: Option<&Type>,
    struct_names: &[&str],
    enum_names: &[&str],
    override_msg_type: Option<Type>,
    enclosing_type: Option<&TypeIdentifier>,
    impl_type_params: &[TypeParam],
    specialized_args: Option<&[Type]>,
) {
    let sig = lookup_sig(&f.name, ctx, enclosing_type, specialized_args);

    let mut env: HashMap<String, VarInfo> = HashMap::new();

    if let Some(ty) = self_type {
        env.insert(
            "self".to_string(),
            VarInfo {
                ty: ty.clone(),
                state: VarState::Live,
            },
        );
    }

    if let Some(sig) = &sig {
        for pi in &sig.params {
            env.insert(
                pi.name.clone(),
                VarInfo {
                    ty: pi.ty.clone(),
                    state: VarState::Live,
                },
            );
        }
    } else {
        let known_packages: BTreeSet<Package> = ctx.package_types.keys().cloned().collect();
        for param in &f.params {
            if let Param::Regular {
                name, type_expr, ..
            } = param
            {
                let ty = resolve_type_expr_with_params(
                    type_expr,
                    struct_names,
                    enum_names,
                    &[],
                    &BTreeMap::new(),
                    &known_packages,
                );
                env.insert(
                    name.clone(),
                    VarInfo {
                        ty,
                        state: VarState::Live,
                    },
                );
            }
        }
    }

    let declared_return = sig.map(|s| s.return_type.clone()).unwrap_or_else(|| {
        f.return_type
            .as_ref()
            .map(|te| {
                let known_packages: BTreeSet<Package> = ctx.package_types.keys().cloned().collect();
                resolve_type_expr_with_params(
                    te,
                    struct_names,
                    enum_names,
                    &[],
                    &BTreeMap::new(),
                    &known_packages,
                )
            })
            .unwrap_or(Type::Unit)
    });

    let is_extern_c = f.annotations.iter().any(|a| {
        a.name == "extern" && matches!(&a.value, Some(AnnotationValue::String(s)) if s == "C")
    });

    if is_extern_c {
        if f.body.is_some() {
            ctx.error(
                "`@extern \"C\"` functions must not have a body".to_string(),
                f.span,
            );
        }
        validate_ffi_signature(f, ctx);
        return;
    }

    if f.body.is_none() {
        ctx.error(
            "function has no body — did you mean to add `@extern \"C\"`?".to_string(),
            f.span,
        );
        return;
    }

    let body = f.body.as_mut().unwrap();
    if body.is_empty() {
        return;
    }

    let kind = f
        .params
        .iter()
        .find_map(|p| match p {
            Param::Self_ { mode, .. } => Some(FunctionKind::Instance(*mode)),
            _ => None,
        })
        .unwrap_or(FunctionKind::Static);

    let process_msg_type = override_msg_type;

    let mut ce = CheckEnv {
        env,
        used_vars: HashSet::new(),
        loop_depth: 0,
        return_type: declared_return.clone(),
        kind,
        struct_names,
        enum_names,
        type_hint: None,
        process_msg_type,
        fn_type_params: impl_type_params
            .iter()
            .chain(f.type_params.iter())
            .cloned()
            .collect(),
        enclosing_type: enclosing_type.cloned(),
        enclosing_specialization: specialized_args.map(|args| args.to_vec()),
    };

    let check_implicit_return = declared_return != Type::Unit && declared_return != Type::Unknown;
    let last_is_expr = matches!(body.last(), Some(Statement::Expr(_)));

    if check_implicit_return && last_is_expr {
        let len = body.len();
        check_body(&mut body[..len - 1], ctx, &mut ce);
        if let Some(Statement::Expr(expr)) = body.last_mut() {
            let actual = infer_expr(expr, ctx, &mut ce);
            if actual.is_known()
                && !types_compatible(&actual, &declared_return)
                && !is_diverging(expr)
            {
                ctx.error_with_hint(
                    format!(
                        "return type mismatch: expected `{}`, found `{}`",
                        declared_return.display(),
                        actual.display()
                    ),
                    format!(
                        "function is declared to return `{}`",
                        declared_return.display()
                    ),
                    expr_span(expr),
                );
            } else if actual.is_known() {
                record_coercion_if_needed(&actual, &declared_return, expr_span(expr), ctx);
            }
        }
    } else {
        check_body(body, ctx, &mut ce);
    }
}

/// Validates that an `@extern "C"` function's parameter and return types are
/// FFI-compatible (explicit-width primitives only).
fn validate_ffi_signature(f: &Function, ctx: &mut TypeContext) {
    for param in &f.params {
        if let Param::Self_ { span, .. } = param {
            ctx.error(
                "`@extern \"C\"` functions cannot have a `self` parameter".to_string(),
                *span,
            );
            continue;
        }
        if let Param::Regular {
            type_expr, span, ..
        } = param
        {
            check_ffi_type_expr(type_expr, *span, ctx);
        }
    }
    if let Some(ret) = &f.return_type {
        check_ffi_type_expr(ret, f.span, ctx);
    }
}

fn check_ffi_type_expr(te: &TypeExpr, span: Span, ctx: &mut TypeContext) {
    match te {
        TypeExpr::Named { path, .. } if path.len() == 1 => {
            let name = &path[0];
            match name.as_str() {
                "Int8" | "Int16" | "Int32" | "Int64" | "UInt8" | "UInt16" | "UInt32"
                | "UInt64" | "Float32" | "Float64" | "Bool" => {}
                "Int" => ctx.error(
                    "type `Int` is not allowed in `@extern \"C\"` functions — use `Int64` for the explicit 64-bit type".to_string(),
                    span,
                ),
                "Float" => ctx.error(
                    "type `Float` is not allowed in `@extern \"C\"` functions — use `Float64` for the explicit 64-bit type".to_string(),
                    span,
                ),
                "String" => ctx.error(
                    "type `String` is not FFI-compatible — use `CPtr<UInt8>` with `CString`".to_string(),
                    span,
                ),
                other => ctx.error(
                    format!("type `{other}` is not FFI-compatible in `@extern \"C\"` functions"),
                    span,
                ),
            }
        }
        TypeExpr::Generic { path, args, .. }
            if path.len() == 1 && path[0] == "CPtr" && args.len() == 1 => {}
        TypeExpr::Unit { .. } => {}
        _ => ctx.error(
            "only explicit-width primitive types and `CPtr<T>` are allowed in `@extern \"C\"` functions"
                .to_string(),
            span,
        ),
    }
}

/// Validates that call arguments match the expected parameter count and types,
/// emitting diagnostics for arity mismatches or type mismatches.
pub(crate) fn check_call_args(
    display_name: &str,
    params: &[ParamInfo],
    args: &mut [Arg],
    sig_prefix: &str,
    span: Span,
    ctx: &mut TypeContext,
    ce: &mut CheckEnv,
) {
    if params.len() != args.len() {
        let param_list: Vec<String> = params
            .iter()
            .map(|p| format!("{}: {}", p.name, p.ty.display()))
            .collect();
        ctx.error_with_hint(
            format!(
                "function `{}` expects {} argument(s), got {}",
                display_name,
                params.len(),
                args.len()
            ),
            format!(
                "signature: fn {}({}{})",
                display_name,
                sig_prefix,
                param_list.join(", ")
            ),
            span,
        );
    } else {
        for (i, arg) in args.iter_mut().enumerate() {
            let param = &params[i];
            let arg_ty = infer_expr_with_expected(&mut arg.value, Some(&param.ty), ctx, ce);
            if param.ty.is_known() && arg_ty.is_known() {
                if !types_compatible(&arg_ty, &param.ty) {
                    ctx.error(
                        format!(
                            "argument `{}`: expected `{}`, found `{}`",
                            param.name,
                            param.ty.display(),
                            arg_ty.display()
                        ),
                        arg.span,
                    );
                } else {
                    record_coercion_if_needed(&arg_ty, &param.ty, arg.span, ctx);
                }
            }
            if param.mode == PassMode::Move
                && !arg_ty.is_copy()
                && let ExprKind::Ident { name, .. } = &arg.value.kind
            {
                ce.mark_moved(name, arg.span);
            }
        }
    }
}

/// Compares actual vs expected type and reports a diagnostic on mismatch.
pub(crate) fn check_type(actual: &Type, expected: &Type, span: Span, ctx: &mut TypeContext) {
    if !actual.is_known() || !expected.is_known() {
        return;
    }
    if !types_compatible(actual, expected) {
        ctx.error(
            format!(
                "type mismatch: expected `{}`, found `{}`",
                expected.display(),
                actual.display()
            ),
            span,
        );
    }
}

/// Attempts to parse a mangled generic name (e.g. `Pair_$i32.i32$`) back into
/// the base name and concrete type arguments for method resolution.
pub(crate) fn try_parse_mangled_generic(
    name: &str,
    ctx: &TypeContext,
) -> Option<(String, Vec<Type>)> {
    let sep_pos = name.find("_$")?;
    let base = &name[..sep_pos];
    ctx.resolve_name(base)?;
    if !name.ends_with('$') {
        return None;
    }
    let inner = &name[sep_pos + 2..name.len() - 1];
    let parts = split_mangled_args(inner);
    let type_args: Vec<Type> = parts
        .iter()
        .map(|s| {
            if let Some(p) = Primitive::from_name(s) {
                Type::Primitive(p)
            } else if s == "unit" {
                Type::Unit
            } else if let Some(id) = ctx.resolve_name(s) {
                Type::Named {
                    identifier: id.clone(),
                    type_args: vec![],
                }
            } else {
                named(s)
            }
        })
        .collect();
    Some((base.to_string(), type_args))
}

/// Splits a mangled args string on `.` at depth 0, respecting nested `_$...$`.
fn split_mangled_args(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'_' && bytes[i + 1] == b'$' {
            depth += 1;
            current.push('_');
            current.push('$');
            i += 2;
        } else if bytes[i] == b'$' {
            depth -= 1;
            current.push('$');
            i += 1;
        } else if bytes[i] == b'.' && depth == 0 {
            parts.push(std::mem::take(&mut current));
            i += 1;
        } else {
            current.push(bytes[i] as char);
            i += 1;
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Returns `true` when `expr` is a call to a diverging function (e.g. `panic`)
/// whose return type should be treated as compatible with any declared type.
pub(crate) fn is_diverging(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::Call { callee, .. }
            if matches!(&callee.kind, ExprKind::Ident { name, .. } if name == "panic")
    )
}

/// Checks if two types are compatible, accounting for numeric coercion and
/// generic instances with partially-known type arguments.
pub(crate) fn types_compatible(a: &Type, b: &Type) -> bool {
    let a = match a {
        Type::Indirect(inner) => inner.as_ref(),
        other => other,
    };
    let b = match b {
        Type::Indirect(inner) => inner.as_ref(),
        other => other,
    };
    if a == b || numeric_compatible(a, b) {
        return true;
    }
    // A concrete type is compatible with a union if it's one of the constituents (widening)
    if let Type::Union(members) = b {
        return members.iter().any(|m| types_compatible(a, m));
    }
    // Two unions are compatible if they have the same canonical members
    if let (Type::Union(ma), Type::Union(mb)) = (a, b) {
        return ma == mb;
    }
    if let (
        Type::Named {
            identifier: ia,
            type_args: ta,
        },
        Type::Named {
            identifier: ib,
            type_args: tb,
        },
    ) = (a, b)
    {
        return ia.name == ib.name
            && ta.len() == tb.len()
            && ta
                .iter()
                .zip(tb.iter())
                .all(|(x, y)| !x.is_known() || !y.is_known() || x == y);
    }
    false
}

/// If `target` is a union and `source` is a non-union constituent, records a
/// widening coercion so the codegen can emit the tag+payload wrapper.
pub(crate) fn record_coercion_if_needed(
    source: &Type,
    target: &Type,
    span: Span,
    ctx: &mut TypeContext,
) {
    if let Type::Union(members) = target
        && !matches!(source, Type::Union(_))
        && members.iter().any(|m| types_compatible(source, m))
    {
        ctx.coercions.insert(
            span,
            Coercion::UnionWiden {
                source: source.clone(),
                target: target.clone(),
            },
        );
    }
}

/// Checks whether a literal integer value (possibly negated) fits in the given
/// bit width, emitting a diagnostic on overflow.
pub(crate) fn check_literal_overflow(
    value_expr: &Expr,
    bits: u64,
    signedness: Option<BinarySignedness>,
    span: Span,
    ctx: &mut TypeContext,
) {
    if bits == 0 || bits > 64 {
        return;
    }

    let val = match &value_expr.kind {
        ExprKind::Literal {
            value: Literal::Int(n),
        } => n.parse::<i128>().ok(),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => {
            if let ExprKind::Literal {
                value: Literal::Int(n),
            } = &operand.kind
            {
                n.parse::<i128>().ok().map(|v| -v)
            } else {
                None
            }
        }
        _ => None,
    };

    let Some(val) = val else { return };

    let is_signed = signedness == Some(BinarySignedness::Signed);
    if is_signed {
        let min = -(1i128 << (bits - 1));
        let max = (1i128 << (bits - 1)) - 1;
        if val < min || val > max {
            ctx.error(
                format!("{val} does not fit in {bits} signed bits (range {min}..{max})"),
                span,
            );
        }
    } else {
        let max = if bits >= 128 {
            i128::MAX
        } else {
            (1i128 << bits) - 1
        };
        if val < 0 || val > max {
            ctx.error(
                format!("{val} does not fit in {bits} unsigned bits (range 0..{max})"),
                span,
            );
        }
    }
}
