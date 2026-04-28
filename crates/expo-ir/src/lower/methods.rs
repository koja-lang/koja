//! Lowering for method-call signature resolution.
//!
//! Threads the call-site type arguments through `Self`, computes the
//! method's mangled symbol, and reports the resolved parameter / return
//! types so emission can build the LLVM call directly.

use std::collections::HashMap;

use expo_ast::ast::{Arg, Expr, ExprKind, ImplMember, TypeExpr, TypeParam};
use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::context::{FunctionKind, PassMode};
use expo_typecheck::types::{
    Type, build_substitution, mangle_method_suffix, mangle_name, named_generic,
    resolve_type_alias_id, resolve_type_alias_name, substitute,
};

use crate::Lowerer;
use crate::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier};
use crate::lower::LowerCtx;
use crate::lower::calls::{args_need_coercion, receiver_variable_name};
use crate::lower::inference::{infer_method_type_args, lookup_method_type_params};
use crate::lower::naming::method_symbol_prefix;
use crate::lower::stmt::resolve_coercion;
use crate::lower::structs::resolve_struct_name;
use crate::lower::types::{find_type_current, id_for, resolve_name_current};
use crate::program::IRProgram;
use crate::resolved::calls::{PendingMethodMono, ResolvedMethodCall};
use crate::resolved::methods::ResolvedMethodSignature;
use crate::values::{IRInstruction, IROperand};

/// Resolves the method signature for a generic impl method by looking up
/// the AST (specialized or generic path), building type substitutions,
/// and computing parameter / return types. No LLVM emission.
///
/// Always returns the resolved signature on success; idempotency against
/// the program-level callable registry is the caller's responsibility
/// (see [`crate::lower::monomorphize::monomorphize_impl_method`]).
pub fn resolve_method_signature(
    ctx: &LowerCtx<'_>,
    base_type: &str,
    method_name: &str,
    type_args: &[Type],
    method_type_args: &[Type],
) -> Result<ResolvedMethodSignature, String> {
    let base_id = resolve_name_current(ctx, base_type)
        .cloned()
        .ok_or_else(|| format!("cannot resolve package for generic method base `{base_type}`"))?;
    let mangled_type = MonomorphizedTypeIdentifier::new(mangle_name(&base_id, type_args));
    let mangled_fn = if method_type_args.is_empty() {
        FunctionIdentifier::new(format!("{mangled_type}_{method_name}"))
    } else {
        let mangled_method = mangle_method_suffix(method_name, method_type_args);
        FunctionIdentifier::new(format!("{mangled_type}_{mangled_method}"))
    };

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

    Ok(ResolvedMethodSignature {
        func_ast,
        is_static,
        mangled_fn,
        mangled_type,
        param_types,
        return_type,
        self_type,
        subst,
    })
}

/// Resolves the call target for `receiver.method(args)`: chooses the
/// mangled callee symbol, computes the parameter / return types
/// (substituting generic type-params when applicable), and reports
/// whether the receiver is consumed by-move.
///
/// LLVM-free: when the call is on a generic receiver and the symbol
/// isn't yet emitted, the resolver records a [`PendingMethodMono`] so
/// the caller can drive `monomorphize_impl_method` (which handles
/// stdlib intrinsic dispatch + IR planning + LLVM emission) before
/// looking up the `FunctionValue`.
///
/// `var_type` is a closure bridge to `Compiler.fn_state.variables` for
/// argument-driven type-arg inference; idempotency for monomorphization
/// is keyed on `program.contains_function(...)`, the canonical
/// callable-symbol registry on [`IRProgram`].
#[allow(clippy::too_many_arguments)]
pub fn resolve_method_call(
    ctx: &LowerCtx<'_>,
    program: &IRProgram,
    var_type: &dyn Fn(&str) -> Option<Type>,
    struct_name: &str,
    base: &str,
    type_id: Option<&TypeIdentifier>,
    type_args: &[Type],
    method: &str,
    args: &[Arg],
) -> Result<ResolvedMethodCall, String> {
    let resolved_id = id_for(ctx, base, type_id);
    let is_generic = !type_args.is_empty();

    // Pick the symbol prefix in lockstep with definition-site mangling:
    //   * non-generic types with a resolved package -> `{pkg}.{TypeName}` for
    //     user packages, plain `{TypeName}` for stdlib/primitives;
    //   * generics continue to use the existing bare-name mangled key until
    //     registration migrates in a later stage.
    let symbol_prefix = if is_generic {
        struct_name.to_string()
    } else {
        resolved_id
            .as_ref()
            .map(|id| method_symbol_prefix(&id.package, &id.name))
            .unwrap_or_else(|| struct_name.to_string())
    };

    let mut mangled = format!("{symbol_prefix}_{method}");
    let mut resolved_method_type_args: Vec<Type> = Vec::new();
    let mut pending_mono: Option<PendingMethodMono> = None;

    if is_generic {
        let method_type_params = lookup_method_type_params(ctx, base, method);

        if !method_type_params.is_empty() {
            let method_type_args =
                infer_method_type_args(ctx, var_type, base, method, type_args, args)?;
            resolved_method_type_args = method_type_args.clone();
            let method_suffix = mangle_method_suffix(method, &method_type_args);
            mangled = format!("{symbol_prefix}_{method_suffix}");

            if !program.contains_function(&FunctionIdentifier::new(&mangled)) {
                pending_mono = Some(PendingMethodMono {
                    base_type: base.to_string(),
                    method: method.to_string(),
                    type_args: type_args.to_vec(),
                    method_type_args,
                });
            }
        } else if !program.contains_function(&FunctionIdentifier::new(&mangled)) {
            pending_mono = Some(PendingMethodMono {
                base_type: base.to_string(),
                method: method.to_string(),
                type_args: type_args.to_vec(),
                method_type_args: Vec::new(),
            });
        }
    }

    let (param_types, return_type) = if let Some(sig) = ctx.type_ctx.function_sig(&mangled) {
        (
            sig.params.iter().map(|p| p.ty.clone()).collect(),
            sig.return_type.clone(),
        )
    } else if is_generic
        && let Some(ti) = resolved_id
            .as_ref()
            .and_then(|id| ctx.type_ctx.get_type(id))
        && let Some(sig) = ti.functions.get(method)
    {
        let mut subst = build_substitution(&ti.type_params, type_args);
        for (mp, ma) in sig.type_params.iter().zip(resolved_method_type_args.iter()) {
            subst.insert(mp.name.clone(), ma.clone());
        }
        (
            sig.params
                .iter()
                .map(|p| substitute(&p.ty, &subst))
                .collect(),
            substitute(&sig.return_type, &subst),
        )
    } else if let Some(ti) = resolved_id
        .as_ref()
        .and_then(|id| ctx.type_ctx.get_type(id))
        && let Some(sig) = ti.functions.get(method)
    {
        (
            sig.params.iter().map(|p| p.ty.clone()).collect(),
            sig.return_type.clone(),
        )
    } else if is_generic
        && let Some(spec_id) = resolved_id.as_ref()
        && let Some(entries) = ctx.type_ctx.specialized_methods.get(spec_id)
        && let Some((_, sigs)) = entries.iter().find(|(a, _)| *a == type_args)
        && let Some(sig) = sigs.get(method)
    {
        (
            sig.params.iter().map(|p| p.ty.clone()).collect(),
            sig.return_type.clone(),
        )
    } else {
        (Vec::new(), Type::Unknown)
    };

    let is_move = resolved_id
        .as_ref()
        .and_then(|id| ctx.type_ctx.get_type(id))
        .and_then(|ti| ti.functions.get(method))
        .is_some_and(|sig| sig.kind == FunctionKind::Instance(PassMode::Move));

    Ok(ResolvedMethodCall {
        mangled_name: FunctionIdentifier::new(mangled),
        param_types,
        return_type,
        is_move,
        pending_mono,
    })
}

impl<'a> Lowerer<'a> {
    /// Attempt to lift a `receiver.method(args)` call to an
    /// [`IRInstruction::MethodCall`]. Returns the produced operand
    /// and the resolved return type, or `None` for cases that defer
    /// to [`IRInstruction::Stub`].
    ///
    /// Defers to Stub when:
    ///
    /// - The receiver is an [`ExprKind::Ident`] resolving to a known
    ///   type -- that's a static call, handled by the wrapper's
    ///   legacy path (the static-call lift helper requires the same
    ///   resolved-type lookup the codegen wrapper already performs).
    /// - The receiver expression has no resolved type (the lift
    ///   needs the receiver's static Expo type to compute the
    ///   mangled callee symbol; without it, defer to Stub).
    /// - The method is `clone` with no args (the legacy path
    ///   short-circuits this to a value passthrough that bypasses
    ///   the call).
    /// - The receiver type resolves to a field-typed-as-function
    ///   closure invocation (the legacy path drops into
    ///   [`crate::lower::values::lower_expr_to_operand`]'s closure
    ///   emission via `compile_field_access`).
    /// - The resolved call is self-tail-recursive (TCO continues to
    ///   live in the codegen wrapper -- the lift would lose the
    ///   loop-jump rewrite).
    /// - [`resolve_method_call`] returns `pending_mono`: the method's
    ///   monomorphization driver lives in `expo-codegen` and must
    ///   register the symbol before the lift is safe.
    /// - The resolved mangled symbol isn't yet registered in
    ///   [`IRProgram`] (consistency check against `pending_mono`).
    pub fn lower_method_call_or_stub(
        &mut self,
        instructions: &mut Vec<IRInstruction>,
        receiver: &Expr,
        method: &str,
        args: &[Arg],
    ) -> Option<(IROperand, Type)> {
        if method == "clone" && args.is_empty() {
            return None;
        }
        if args_need_coercion(self, args) {
            return None;
        }
        // Receiver coercion would be applied at materialization time;
        // bail conservatively when the receiver has any recorded
        // coercion so the legacy `compile_method_call` path handles it.
        if resolve_coercion(&self.ctx(), receiver.span).is_some() {
            return None;
        }

        if let ExprKind::Ident { name, .. } = &receiver.kind {
            let alias_name = resolve_type_alias_name(name, &self.type_ctx.type_aliases);
            let resolved_id = resolve_type_alias_id(name, &self.type_ctx.type_aliases)
                .or_else(|| resolve_name_current(&self.ctx(), &alias_name).cloned());
            if let Some(ref id) = resolved_id
                && self.type_ctx.get_type(id).is_some()
            {
                return self.lower_static_call_or_stub(
                    instructions,
                    &alias_name,
                    Some(id),
                    method,
                    args,
                );
            }
        }

        let recv_type = receiver.resolved_type.as_ref()?;

        let resolved_name =
            resolve_struct_name(&self.ctx(), receiver, recv_type, |_| None, None).ok()?;

        let has_impl_method = resolved_name
            .identifier
            .as_ref()
            .filter(|id| id.package != expo_typecheck::types::Package::Unresolved)
            .or_else(|| resolve_name_current(&self.ctx(), &resolved_name.base))
            .and_then(|id| self.type_ctx.get_type(id))
            .and_then(|ti| ti.functions.get(method))
            .is_some();
        if !has_impl_method {
            return None;
        }

        let resolved = resolve_method_call(
            &self.ctx(),
            self.program,
            &|_| None,
            resolved_name.mangled.as_str(),
            &resolved_name.base,
            resolved_name.identifier.as_ref(),
            &resolved_name.type_args,
            method,
            args,
        )
        .ok()?;

        if resolved.pending_mono.is_some() {
            return None;
        }
        if !self.program.contains_function(&resolved.mangled_name) {
            return None;
        }
        // Defer to Stub whenever the call is in tail position so the
        // legacy `compile_method_call` path keeps owning the
        // self-tail-recursive jump rewrite (`loop_header` branch +
        // `param_allocas` store). The wrapper-level lift attempt runs
        // before its own `save_tail`, so `tail_position()` here still
        // reflects the surrounding tail status.
        if self.fn_state.is_self_tail_call(
            resolved.mangled_name.as_str(),
            self.fn_state.tail_position(),
        ) {
            return None;
        }

        let receiver_operand = self.lower_expr_to_operand(instructions, receiver);
        let lowered_args: Vec<IROperand> = args
            .iter()
            .map(|arg| self.lower_expr_to_operand(instructions, &arg.value))
            .collect();

        let dest = self.next_value_id();
        let return_type = resolved.return_type.clone();
        instructions.push(IRInstruction::MethodCall {
            dest,
            mangled: resolved.mangled_name,
            receiver: receiver_operand,
            receiver_name: receiver_variable_name(receiver),
            is_move: resolved.is_move,
            args: lowered_args,
            param_types: resolved.param_types,
            return_type: resolved.return_type,
        });
        Some((IROperand::Local(dest), return_type))
    }
}
