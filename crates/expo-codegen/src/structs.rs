//! Struct compilation: field access, struct construction, and method calls on
//! struct instances.
//!
//! Construction and field access both follow the lower/emit split established
//! by `control/patterns.rs` and mirrored in `enums.rs`.
//!
//! ## Construction
//!
//! - [`lower_struct_construction`] consumes the AST `FieldInit`s plus the
//!   type-checker's resolved identifier and produces a
//!   [`ResolvedStructConstruction`]. All struct lookup, package-aware name
//!   resolution, generic monomorphization, and `unify`-driven type-arg
//!   inference happens here. Lower is the only side that touches
//!   `compiler.types`, `compiler.type_ctx`, or `monomorphize_struct`.
//!
//! - [`emit_struct_construction`] consumes the resolved IR plus the AST
//!   data and emits LLVM IR (alloca, GEP, store). Coercion and per-field
//!   type-substitution context push/pop also live here, since they need a
//!   live function context.
//!
//! [`compile_struct_construction`] is the public entry point and a thin
//! shim. For generics it pre-compiles the field initializers so lower can
//! drive `unify` over their resolved types before triggering monomorphization
//! -- see the design note in `expo/design/EXPOIR.md` for why the boundary
//! relaxes here vs. patterns.
//!
//! ## Field access
//!
//! - [`resolve_chain_steps`] (already IR-only) produces a [`ResolvedChain`]
//!   for variable / `self` / nested-field receivers.
//! - [`lower_value_struct_field`] handles arbitrary receiver expressions,
//!   returning a [`ResolvedFieldStep`]. It tries `receiver.resolved_type`
//!   first (semantic), then falls back to the LLVM struct-name lookup via
//!   `get_mono_field_index` / `get_mono_field_type`.
//! - [`emit_chain_field_access`] walks the resolved chain with GEPs and a
//!   final `load_maybe_indirect`. [`emit_value_struct_field_access`]
//!   allocas, stores the receiver value, GEPs the field, and loads.
//!
//! [`compile_field_access`] is the public entry point and a thin dispatcher
//! between the static-chain and dynamic-receiver paths.
//!
//! Method calls and static calls still mix concerns -- separate future
//! targets.

use std::collections::HashMap;

use expo_ast::ast::{Arg, Expr, ExprKind, FieldInit};
use expo_ir::lower::LowerCtx;
use expo_ir::lower::fields::{lower_struct_field, resolve_chain_steps};
use expo_ir::lower::inference::infer_static_method_return_type as ir_infer_static_method_return_type;
use expo_ir::lower::structs::{lower_concrete_struct, resolve_struct_name};
use expo_ir::lower::types::resolve_name_current;
use expo_ir::resolved::construction::ResolvedStructConstruction;
use expo_ir::resolved::fields::{ResolvedChain, ResolvedFieldStep, ResolvedStructField};
use expo_typecheck::context::TypeInfo;
use expo_typecheck::types::{
    Package, Type, TypeIdentifier, mangle_name, named_generic, resolve_type_alias_id,
    resolve_type_alias_name, substitute, unify, unwrap_indirect,
};
use inkwell::AddressSpace;
use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PointerValue,
};

use crate::calls::invoke_closure_fat_ptr;
use crate::compiler::{Compiler, ExprResult, TypedValue};
use crate::expr::{compile_expr, compile_expr_coerced};
use crate::generics::{
    ensure_types_exist, monomorphize_enum, monomorphize_impl_method, monomorphize_struct,
};
use crate::types::to_llvm_type;
use expo_ir::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier};

/// Loads a value from `field_ptr`. When `field_type` is [`Type::Indirect`],
/// loads the heap pointer first, then dereferences it to get the inner value.
pub(crate) fn load_maybe_indirect<'ctx>(
    c: &mut Compiler<'ctx>,
    field_ptr: PointerValue<'ctx>,
    field_type: &Type,
    label: &str,
) -> BasicValueEnum<'ctx> {
    if let Type::Indirect(inner) = field_type {
        let ptr_ty = c.context.ptr_type(AddressSpace::default());
        let heap_ptr = c
            .builder
            .build_load(ptr_ty, field_ptr, &format!("{label}_ptr"))
            .unwrap()
            .into_pointer_value();
        let _ = ensure_types_exist(c, inner);
        let inner_llvm_ty = to_llvm_type(inner, c.context, &c.llvm_types)
            .expect("indirect inner type must have LLVM representation");
        c.builder
            .build_load(inner_llvm_ty, heap_ptr, &format!("{label}_deref"))
            .unwrap()
    } else {
        let llvm_ty = to_llvm_type(field_type, c.context, &c.llvm_types)
            .unwrap_or_else(|| c.context.i8_type().into());
        c.builder.build_load(llvm_ty, field_ptr, label).unwrap()
    }
}

/// Stores `val` into `field_ptr`. When `field_type` is [`Type::Indirect`],
/// heap-allocates storage via `malloc`, writes the value there, and stores the
/// resulting pointer into `field_ptr`.
pub(crate) fn store_maybe_indirect<'ctx>(
    c: &mut Compiler<'ctx>,
    field_ptr: PointerValue<'ctx>,
    val: BasicValueEnum<'ctx>,
    field_type: &Type,
    label: &str,
) {
    if let Type::Indirect(inner) = field_type {
        let _ = ensure_types_exist(c, inner);
        let inner_llvm_ty = to_llvm_type(inner, c.context, &c.llvm_types)
            .expect("indirect inner type must have LLVM representation");
        let size = llvm_type_size(inner_llvm_ty, c);
        let malloc_fn = *c
            .functions
            .get(&FunctionIdentifier::new("malloc"))
            .expect("malloc not declared");
        let heap_ptr = c
            .call(malloc_fn, &[size.into()], &format!("{label}_malloc"))
            .unwrap()
            .into_pointer_value();
        c.builder.build_store(heap_ptr, val).unwrap();
        c.builder.build_store(field_ptr, heap_ptr).unwrap();
    } else {
        c.builder.build_store(field_ptr, val).unwrap();
    }
}

fn llvm_type_size<'ctx>(ty: BasicTypeEnum<'ctx>, c: &Compiler<'ctx>) -> IntValue<'ctx> {
    match ty {
        BasicTypeEnum::StructType(st) => st
            .size_of()
            .unwrap_or_else(|| c.context.i64_type().const_int(8, false)),
        BasicTypeEnum::IntType(it) => it.size_of(),
        BasicTypeEnum::FloatType(ft) => ft.size_of(),
        BasicTypeEnum::PointerType(pt) => pt.size_of(),
        BasicTypeEnum::ArrayType(at) => at
            .size_of()
            .unwrap_or_else(|| c.context.i64_type().const_int(8, false)),
        BasicTypeEnum::VectorType(vt) => vt
            .size_of()
            .unwrap_or_else(|| c.context.i64_type().const_int(8, false)),
        BasicTypeEnum::ScalableVectorType(svt) => svt
            .size_of()
            .unwrap_or_else(|| c.context.i64_type().const_int(8, false)),
    }
}

/// Compiles a field access expression (`receiver.field`). Thin dispatcher:
/// uses the static-chain path (variable / `self` / nested) when
/// [`resolve_chain_steps`] succeeds; otherwise falls back to compiling the
/// receiver and walking through [`lower_value_struct_field`] +
/// [`emit_value_struct_field_access`].
pub fn compile_field_access<'ctx>(
    compiler: &mut Compiler<'ctx>,
    receiver: &Expr,
    field: &str,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    if let Some(chain) = resolve_chain_steps(&compiler.lower_ctx(), receiver, field, &|name| {
        compiler
            .fn_state
            .variables
            .get(name)
            .map(|(_, ty, _)| ty.clone())
    }) && let Some(result) = emit_chain_field_access(compiler, &chain, field)
    {
        return result;
    }

    let recv_tv = compile_expr(compiler, receiver, function)?
        .ok_or("field access on expression that produced no value")?;
    let step = lower_value_struct_field(compiler, receiver, &recv_tv, field)?;
    emit_value_struct_field_access(compiler, recv_tv, &step, field)
}

/// Resolves the field index/type for a value-struct receiver. Tries the
/// type-checker's resolved type first (package-qualified, so it avoids the
/// shared bare-name index), then falls back to looking up the LLVM struct
/// name attached to the compiled `StructValue`.
fn lower_value_struct_field(
    compiler: &Compiler,
    receiver: &Expr,
    recv_tv: &TypedValue,
    field: &str,
) -> Result<ResolvedFieldStep, String> {
    if let Some(ref recv_ty) = receiver.resolved_type
        && let Some(step) = lower_struct_field(&compiler.lower_ctx(), recv_ty, field)
    {
        return Ok(step);
    }

    if !recv_tv.value.is_struct_value() {
        return Err("field access on non-struct value".to_string());
    }

    let struct_name = recv_tv
        .value
        .into_struct_value()
        .get_type()
        .get_name()
        .map(|n| n.to_str().unwrap_or("").to_string())
        .ok_or("cannot determine struct type for field access")?;

    let field_index = compiler
        .get_mono_field_index(&struct_name, field)
        .ok_or_else(|| format!("unknown field `{field}` on struct `{struct_name}`"))?;

    let field_type = compiler
        .get_mono_field_type(&struct_name, field)
        .ok_or_else(|| format!("unknown field `{field}` on struct `{struct_name}`"))?;

    Ok(ResolvedFieldStep {
        field_index,
        field_type,
    })
}

/// Emits a static GEP chain for a [`ResolvedChain`] and loads the final
/// field. Returns `None` when an intermediate struct type lacks an LLVM
/// representation, so the shim can retry via the dynamic path.
fn emit_chain_field_access<'ctx>(
    compiler: &mut Compiler<'ctx>,
    chain: &ResolvedChain,
    label: &str,
) -> Option<ExprResult<'ctx>> {
    let (mut ptr, _, _) = compiler.fn_state.variables.get(&chain.base_name).cloned()?;
    let mut current_type = chain.base_type.clone();

    for step in &chain.steps {
        let struct_type =
            to_llvm_type(&current_type, compiler.context, &compiler.llvm_types)?.into_struct_type();
        ptr = compiler
            .builder
            .build_struct_gep(struct_type, ptr, step.field_index, label)
            .unwrap();
        current_type = step.field_type.clone();
    }

    let val = load_maybe_indirect(compiler, ptr, &current_type, label);
    Some(Ok(Some(TypedValue::new(
        val,
        unwrap_indirect(&current_type).clone(),
    ))))
}

/// Emits a field access on a value-struct receiver: alloca a scratch slot,
/// store the receiver into it, GEP the field, and load.
fn emit_value_struct_field_access<'ctx>(
    compiler: &mut Compiler<'ctx>,
    recv_tv: TypedValue<'ctx>,
    step: &ResolvedFieldStep,
    field: &str,
) -> ExprResult<'ctx> {
    if !recv_tv.value.is_struct_value() {
        return Err("field access on non-struct value".to_string());
    }

    let struct_value = recv_tv.value.into_struct_value();
    let struct_llvm_type = struct_value.get_type();
    let tmp_alloca = compiler
        .builder
        .build_alloca(struct_llvm_type, "tmp_struct")
        .unwrap();
    compiler
        .builder
        .build_store(tmp_alloca, struct_value)
        .unwrap();

    let field_ptr = compiler
        .builder
        .build_struct_gep(struct_llvm_type, tmp_alloca, step.field_index, field)
        .unwrap();

    let val = load_maybe_indirect(compiler, field_ptr, &step.field_type, field);
    Ok(Some(TypedValue::new(
        val,
        unwrap_indirect(&step.field_type).clone(),
    )))
}

/// Compiles a method call (`receiver.method(args)`).
/// The resolved method call target. Captures everything needed to emit the
/// call without further type lookups.
struct ResolvedMethodCall<'ctx> {
    callee: FunctionValue<'ctx>,
    is_move: bool,
    mangled_name: String,
    param_types: Vec<Type>,
    return_type: Type,
}

pub fn compile_method_call<'ctx>(
    c: &mut Compiler<'ctx>,
    receiver: &Expr,
    method: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let was_tail = c.fn_lower.save_tail();

    if let ExprKind::Ident { name, .. } = &receiver.kind {
        let resolved = resolve_type_alias_name(name, &c.type_ctx.type_aliases);
        let resolved_id = resolve_type_alias_id(name, &c.type_ctx.type_aliases)
            .or_else(|| resolve_name_current(&c.lower_ctx(), &resolved).cloned());
        if let Some(ref id) = resolved_id
            && c.type_ctx.get_type(id).is_some()
        {
            return compile_static_call(c, &resolved, Some(id), method, args, function);
        }
    }

    let recv_tv = compile_expr(c, receiver, function)?
        .ok_or("method call on expression that produced no value")?;
    let recv_val = recv_tv.value;

    if method == "clone" && args.is_empty() {
        return Ok(Some(recv_tv));
    }

    let llvm_struct_name = if recv_val.is_struct_value() {
        recv_val
            .into_struct_value()
            .get_type()
            .get_name()
            .and_then(|n| n.to_str().ok())
            .map(MonomorphizedTypeIdentifier::new)
    } else {
        None
    };
    let resolved_name = resolve_struct_name(
        &c.lower_ctx(),
        receiver,
        &recv_tv.expo_type,
        |name| c.fn_state.variables.get(name).map(|(_, ty, _)| ty.clone()),
        llvm_struct_name.as_ref(),
    )?;

    let has_impl_method = resolved_name
        .identifier
        .as_ref()
        .filter(|id| id.package != Package::Unresolved)
        .or_else(|| resolve_name_current(&c.lower_ctx(), &resolved_name.base))
        .and_then(|id| c.type_ctx.get_type(id))
        .and_then(|ti| ti.functions.get(method))
        .is_some();
    if !has_impl_method
        && let Some(field_ty) = c.get_mono_field_type(resolved_name.mangled.as_str(), method)
    {
        let inner = unwrap_indirect(&field_ty);
        if let Type::Function {
            params,
            return_type,
        } = inner.clone()
        {
            let field_val = compile_field_access(c, receiver, method, function)?
                .ok_or_else(|| format!("field `{method}` produced no value"))?
                .value;
            let fat = field_val.into_struct_value();
            return invoke_closure_fat_ptr(
                c,
                fat,
                &params,
                return_type.as_ref(),
                args,
                function,
                &format!("field_{method}"),
            );
        }
    }

    let resolved = resolve_method_call(
        c,
        resolved_name.mangled.as_str(),
        &resolved_name.base,
        resolved_name.identifier.as_ref(),
        &resolved_name.type_args,
        method,
        args,
    )?;

    let mut llvm_args: Vec<BasicMetadataValueEnum> = Vec::new();
    llvm_args.push(recv_val.into());

    for (i, arg) in args.iter().enumerate() {
        let val = if i < resolved.param_types.len() {
            let expected = &resolved.param_types[i];
            if matches!(expected, Type::Unit) {
                c.context.i8_type().const_int(0, false).into()
            } else {
                compile_expr_coerced(c, &arg.value, expected, function)?
                    .ok_or_else(|| "method argument produced no value".to_string())?
            }
        } else {
            compile_expr(c, &arg.value, function)?
                .ok_or_else(|| "method argument produced no value".to_string())?
                .value
        };
        llvm_args.push(val.into());
    }

    c.fn_lower.restore_tail(was_tail);

    let is_tail = c
        .fn_lower
        .is_self_tail_call(&resolved.mangled_name, was_tail);

    if is_tail && let Some(loop_header) = c.fn_state.loop_header {
        crate::drop::drop_live_variables(c, Some("self"));
        for (arg, alloca) in llvm_args.iter().zip(c.fn_state.param_allocas.iter()) {
            let val: BasicValueEnum = (*arg).try_into().unwrap();
            c.builder.build_store(*alloca, val).unwrap();
        }
        c.builder.build_unconditional_branch(loop_header).unwrap();
        return Ok(None);
    }

    let result = c.call(
        resolved.callee,
        &llvm_args,
        &format!("{}_ret", resolved.mangled_name),
    );

    if resolved.is_move {
        let recv_name = match &receiver.kind {
            ExprKind::Ident { name, .. } => Some(name.as_str()),
            ExprKind::Self_ => Some("self"),
            _ => None,
        };
        if let Some(name) = recv_name
            && let Some((ptr, ty, _)) = c.fn_state.variables.get(name)
        {
            let entry = (*ptr, ty.clone(), crate::drop::Ownership::Unowned);
            c.fn_state.variables.insert(name.to_string(), entry);
        }
    }

    Ok(result.map(|v| TypedValue::new(v, resolved.return_type)))
}

/// Resolves which method to call by delegating to the LLVM-free
/// resolver in [`expo_ir::lower::methods::resolve_method_call`], then
/// orchestrating any pending monomorphization and the final
/// [`FunctionValue`] lookup. The `'ctx`-bound result struct is kept
/// local since `FunctionValue<'ctx>` cannot live in `expo-ir`.
fn resolve_method_call<'ctx>(
    c: &mut Compiler<'ctx>,
    struct_name: &str,
    base: &str,
    type_id: Option<&TypeIdentifier>,
    type_args: &[Type],
    method: &str,
    args: &[Arg],
) -> Result<ResolvedMethodCall<'ctx>, String> {
    let resolved = {
        let lower_ctx = LowerCtx {
            closure_site_path: c.closure_site_path.as_deref(),
            fn_lower: &c.fn_lower,
            layouts: &c.layouts,
            package: c.current_package.as_ref(),
            type_ctx: c.type_ctx,
        };
        let var_type = |name: &str| c.fn_state.variables.get(name).map(|(_, ty, _)| ty.clone());
        let function_exists = |id: &FunctionIdentifier| c.functions.contains_key(id);
        expo_ir::lower::methods::resolve_method_call(
            &lower_ctx,
            &var_type,
            &function_exists,
            struct_name,
            base,
            type_id,
            type_args,
            method,
            args,
        )?
    };

    if let Some(p) = &resolved.pending_mono
        && !c.functions.contains_key(&resolved.mangled_name)
    {
        monomorphize_impl_method(
            c,
            &p.base_type,
            &p.method,
            &p.type_args,
            &p.method_type_args,
        )?;
    }

    let callee = *c
        .functions
        .get(&resolved.mangled_name)
        .ok_or_else(|| format!("undefined method `{method}` on `{struct_name}`"))?;

    Ok(ResolvedMethodCall {
        callee,
        is_move: resolved.is_move,
        mangled_name: resolved.mangled_name.as_str().to_string(),
        param_types: resolved.param_types,
        return_type: resolved.return_type,
    })
}

/// Infers the return type of a static struct/enum method call (e.g.
/// `Task.async(...)`) for codegen variable typing when there is no
/// annotation. Thin wrapper around the LLVM-free resolver in
/// [`expo_ir::lower::inference`].
pub fn infer_static_method_return_type(
    c: &Compiler,
    type_name: &str,
    method: &str,
    args: &[Arg],
) -> Option<Type> {
    ir_infer_static_method_return_type(
        &c.lower_ctx(),
        &|name: &str| c.fn_state.variables.get(name).map(|(_, ty, _)| ty.clone()),
        type_name,
        method,
        args,
    )
}

/// Temporarily pushes type-parameter substitutions for a [`GenericInstance`]
/// field type so that empty collection literals (`[]`, `{}`) monomorphize to
/// the correct element type instead of falling back to `I32`.
fn push_generic_type_subst<'ctx>(
    c: &mut Compiler<'ctx>,
    field_type: &Type,
) -> Option<HashMap<String, Type>> {
    let ty = unwrap_indirect(field_type);
    if let Type::Named {
        identifier,
        type_args,
    } = ty
        && !type_args.is_empty()
    {
        let type_params = c
            .type_ctx
            .get_type(identifier)
            .map(|ti| ti.type_params.clone())?;
        let saved = c.fn_lower.type_subst.clone();
        for (param, arg) in type_params.iter().zip(type_args.iter()) {
            let concrete = substitute(arg, &c.fn_lower.type_subst);
            c.fn_lower.type_subst.insert(param.name.clone(), concrete);
        }
        Some(saved)
    } else {
        None
    }
}

/// Compiles a struct literal (`StructName { field: value, ... }`). Thin
/// lower/emit shim. For generic structs, pre-compiles the field initializers
/// so [`lower_struct_construction`] can drive `unify` over their resolved
/// types before triggering monomorphization.
pub fn compile_struct_construction<'ctx>(
    compiler: &mut Compiler<'ctx>,
    type_path: &[String],
    fields: &[FieldInit],
    resolved_type: Option<&TypeIdentifier>,
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let raw_name = type_path
        .first()
        .ok_or("empty type path in struct construction")?;

    let is_generic = lookup_struct_info(compiler, raw_name, resolved_type)
        .is_some_and(|info| info.is_struct() && !info.type_params.is_empty());

    let pre_compiled = if is_generic {
        precompile_generic_struct_fields(compiler, fields, function)?
    } else {
        PreCompiledFields::default()
    };

    let resolved = lower_struct_construction(
        compiler,
        raw_name,
        fields,
        resolved_type,
        &pre_compiled.types,
    )?;

    emit_struct_construction(compiler, &resolved, fields, &pre_compiled.values, function)
}

/// Pre-compiled field values for the generic struct-construction path,
/// where lower needs the resolved types to drive `unify`.
#[derive(Default)]
struct PreCompiledFields<'ctx> {
    types: Vec<Type>,
    values: Vec<BasicValueEnum<'ctx>>,
}

fn precompile_generic_struct_fields<'ctx>(
    compiler: &mut Compiler<'ctx>,
    fields: &[FieldInit],
    function: FunctionValue<'ctx>,
) -> Result<PreCompiledFields<'ctx>, String> {
    let mut types = Vec::with_capacity(fields.len());
    let mut values = Vec::with_capacity(fields.len());
    for field_init in fields {
        let tv = compile_expr(compiler, &field_init.value, function)?
            .ok_or_else(|| format!("field `{}` produced no value", field_init.name))?;
        types.push(infer_field_init_type(
            compiler,
            &field_init.value,
            &tv.expo_type,
        ));
        values.push(tv.value);
    }
    Ok(PreCompiledFields { types, values })
}

/// Best-effort field-init type for `unify`. When the compiled value's type is
/// `Unknown` (e.g. closures), looks through identifiers and field accesses to
/// pull a more specific type from the variable scope.
fn infer_field_init_type(compiler: &Compiler, expr: &Expr, compiled_type: &Type) -> Type {
    if *compiled_type != Type::Unknown {
        return compiled_type.clone();
    }
    match &expr.kind {
        ExprKind::Ident { name, .. } => compiler
            .fn_state
            .variables
            .get(name)
            .map(|(_, ty, _)| ty.clone())
            .unwrap_or(Type::Unknown),
        ExprKind::FieldAccess {
            receiver, field, ..
        } => {
            if let ExprKind::Ident { name, .. } = &receiver.as_ref().kind
                && let Some((_, recv_ty, _)) = compiler.fn_state.variables.get(name)
                && let Some(step) = lower_struct_field(&compiler.lower_ctx(), recv_ty, field)
            {
                substitute(&step.field_type, &compiler.fn_lower.type_subst)
            } else {
                Type::Unknown
            }
        }
        _ => Type::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Lowering
// ---------------------------------------------------------------------------

/// Lowers a struct construction to its resolved IR. Handles both concrete and
/// generic structs uniformly: for generics, runs `unify` over the supplied
/// `compiled_field_types` and triggers monomorphization. The returned
/// `mangled_name` is always the post-monomorphization key suitable for
/// `compiler.llvm_types.get_monomorphized` / `get_concrete`.
fn lower_struct_construction(
    compiler: &mut Compiler,
    raw_name: &str,
    field_inits: &[FieldInit],
    resolved_type: Option<&TypeIdentifier>,
    compiled_field_types: &[Type],
) -> Result<ResolvedStructConstruction, String> {
    let is_generic = lookup_struct_info(compiler, raw_name, resolved_type)
        .is_some_and(|info| info.is_struct() && !info.type_params.is_empty());

    if is_generic {
        let info = lookup_struct_info(compiler, raw_name, resolved_type)
            .cloned()
            .expect("is_generic implies info exists");
        let struct_name = resolve_type_alias_name(raw_name, &compiler.type_ctx.type_aliases);
        return lower_generic_struct(
            compiler,
            &struct_name,
            &info,
            field_inits,
            compiled_field_types,
        );
    }

    lower_concrete_struct(&compiler.lower_ctx(), raw_name, field_inits, resolved_type)
}

fn lookup_struct_info<'a>(
    compiler: &'a Compiler,
    raw_name: &str,
    resolved_type: Option<&TypeIdentifier>,
) -> Option<&'a TypeInfo> {
    let struct_name = resolve_type_alias_name(raw_name, &compiler.type_ctx.type_aliases);
    resolved_type
        .filter(|id| id.package != Package::Unresolved)
        .and_then(|id| compiler.type_ctx.get_type(id))
        .or_else(|| {
            resolve_type_alias_id(raw_name, &compiler.type_ctx.type_aliases)
                .and_then(|id| compiler.type_ctx.get_type(&id))
        })
        .or_else(|| {
            compiler
                .type_ctx
                .resolve_name(&struct_name)
                .and_then(|id| compiler.type_ctx.get_type(id))
        })
}

fn lower_generic_struct(
    compiler: &mut Compiler,
    struct_name: &str,
    info: &TypeInfo,
    field_inits: &[FieldInit],
    compiled_field_types: &[Type],
) -> Result<ResolvedStructConstruction, String> {
    let struct_fields = info
        .fields()
        .ok_or_else(|| format!("internal: generic construction expected struct `{struct_name}`"))?;

    let mut subst = HashMap::new();
    for (i, field_init) in field_inits.iter().enumerate() {
        if let Some((_, field_ty)) = struct_fields.iter().find(|(n, _)| n == &field_init.name) {
            let compiled_type = compiled_field_types
                .get(i)
                .cloned()
                .unwrap_or(Type::Unknown);
            if !unify(field_ty, &compiled_type, &mut subst) {
                return Err(format!(
                    "type mismatch for field `{}` in generic struct `{struct_name}`",
                    field_init.name
                ));
            }
        }
    }

    let type_args: Vec<Type> = info
        .type_params
        .iter()
        .map(|tp| subst.get(&tp.name).cloned().unwrap_or(Type::Unknown))
        .collect();

    // We must have a package-resolved TypeIdentifier here so generic structs
    // from different packages produce distinct mangled LLVM keys.
    let struct_id = resolve_name_current(&compiler.lower_ctx(), struct_name)
        .cloned()
        .ok_or_else(|| format!("cannot resolve package for generic struct `{struct_name}`"))?;
    let mangled_name = mangle_name(&struct_id, &type_args);

    if !compiler
        .llvm_types
        .contains_monomorphized(&MonomorphizedTypeIdentifier::new(&mangled_name))
    {
        monomorphize_struct(compiler, &struct_id, &type_args)?;
    }

    let mut fields = Vec::with_capacity(field_inits.len());
    for field_init in field_inits {
        let index = compiler
            .get_mono_field_index(&mangled_name, &field_init.name)
            .ok_or_else(|| {
                format!(
                    "unknown field `{}` in struct `{struct_name}`",
                    field_init.name
                )
            })?;
        let field_type = compiler
            .get_mono_field_type(&mangled_name, &field_init.name)
            .unwrap_or(Type::Unknown);
        fields.push(ResolvedStructField {
            field_type,
            index,
            name: field_init.name.clone(),
        });
    }

    let result_type = named_generic(
        struct_name,
        type_args,
        compiler.type_ctx,
        compiler.current_package.as_ref(),
    );

    Ok(ResolvedStructConstruction {
        fields,
        is_generic: true,
        mangled_name: MonomorphizedTypeIdentifier::new(&mangled_name),
        result_type,
    })
}

// ---------------------------------------------------------------------------
// Emission
// ---------------------------------------------------------------------------

/// Emits LLVM IR for a lowered struct construction. Allocates the struct and
/// stores each field. For the generic path, callers supply
/// `pre_compiled_values` (already evaluated to drive `unify`); for concrete,
/// the slice is empty and emit walks `field_inits` itself with per-field
/// coercion plus a generic-type-substitution context push/pop.
fn emit_struct_construction<'ctx>(
    compiler: &mut Compiler<'ctx>,
    resolved: &ResolvedStructConstruction,
    field_inits: &[FieldInit],
    pre_compiled_values: &[BasicValueEnum<'ctx>],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let struct_type = lookup_struct_llvm_type(compiler, resolved)?;
    let alloca =
        compiler.build_entry_alloca(struct_type, &format!("{}_tmp", resolved.mangled_name));

    for (i, resolved_field) in resolved.fields.iter().enumerate() {
        let val = if let Some(v) = pre_compiled_values.get(i) {
            *v
        } else {
            compile_field_with_subst(compiler, resolved_field, &field_inits[i], function)?
        };

        let field_ptr = compiler
            .builder
            .build_struct_gep(
                struct_type,
                alloca,
                resolved_field.index,
                &resolved_field.name,
            )
            .unwrap();

        if matches!(resolved_field.field_type, Type::Unknown) {
            compiler.builder.build_store(field_ptr, val).unwrap();
        } else {
            store_maybe_indirect(
                compiler,
                field_ptr,
                val,
                &resolved_field.field_type,
                &resolved_field.name,
            );
        }
    }

    let struct_val = compiler
        .builder
        .build_load(struct_type, alloca, resolved.mangled_name.as_str())
        .unwrap();
    Ok(Some(TypedValue::new(
        struct_val,
        resolved.result_type.clone(),
    )))
}

fn lookup_struct_llvm_type<'ctx>(
    compiler: &Compiler<'ctx>,
    resolved: &ResolvedStructConstruction,
) -> Result<StructType<'ctx>, String> {
    if resolved.is_generic {
        return compiler
            .llvm_types
            .get_monomorphized(&resolved.mangled_name)
            .ok_or_else(|| format!("monomorphized struct `{}` not found", resolved.mangled_name));
    }
    if let Type::Named { identifier, .. } = &resolved.result_type
        && let Some(t) = compiler.llvm_types.get_concrete(identifier)
    {
        return Ok(t);
    }
    Err(format!("unknown struct type: {}", resolved.mangled_name))
}

fn compile_field_with_subst<'ctx>(
    compiler: &mut Compiler<'ctx>,
    resolved_field: &ResolvedStructField,
    field_init: &FieldInit,
    function: FunctionValue<'ctx>,
) -> Result<BasicValueEnum<'ctx>, String> {
    let saved_subst = push_generic_type_subst(compiler, &resolved_field.field_type);
    let coerce_ty = unwrap_indirect(&resolved_field.field_type);
    let val = compile_expr_coerced(compiler, &field_init.value, coerce_ty, function)?
        .ok_or_else(|| format!("field `{}` produced no value", resolved_field.name))?;
    if let Some(saved) = saved_subst {
        compiler.fn_lower.type_subst = saved;
    }
    Ok(val)
}

struct ResolvedStaticCall<'ctx> {
    callee: FunctionValue<'ctx>,
    mangled_name: String,
    param_types: Vec<Type>,
    return_type: Type,
}

/// Resolves a static method call (`Type.method(args)`) by delegating to
/// [`expo_ir::lower::calls::resolve_static_call`] for the LLVM-free
/// decision and then orchestrating type / method monomorphization
/// before the final [`FunctionValue`] lookup. Type monomorphization is
/// driven first so the static method's signature can be built against
/// the concrete LLVM struct.
fn resolve_static_call<'ctx>(
    c: &mut Compiler<'ctx>,
    type_name: &str,
    resolved_type: Option<&TypeIdentifier>,
    method: &str,
    args: &[Arg],
) -> Result<ResolvedStaticCall<'ctx>, String> {
    let resolved = {
        let lower_ctx = LowerCtx {
            closure_site_path: c.closure_site_path.as_deref(),
            fn_lower: &c.fn_lower,
            layouts: &c.layouts,
            package: c.current_package.as_ref(),
            type_ctx: c.type_ctx,
        };
        let var_type = |name: &str| c.fn_state.variables.get(name).map(|(_, ty, _)| ty.clone());
        let function_exists = |id: &FunctionIdentifier| c.functions.contains_key(id);
        let type_mono_exists =
            |id: &MonomorphizedTypeIdentifier| c.llvm_types.contains_monomorphized(id);
        expo_ir::lower::calls::resolve_static_call(
            &lower_ctx,
            &var_type,
            &function_exists,
            &type_mono_exists,
            type_name,
            resolved_type,
            method,
            args,
        )?
    };

    if let Some(t) = &resolved.pending_type_mono {
        let mangled = MonomorphizedTypeIdentifier::new(mangle_name(&t.identifier, &t.type_args));
        if !c.llvm_types.contains_monomorphized(&mangled) {
            if t.is_enum {
                monomorphize_enum(c, &t.identifier, &t.type_args)?;
            } else {
                monomorphize_struct(c, &t.identifier, &t.type_args)?;
            }
        }
    }

    if let Some(p) = &resolved.pending_mono
        && !c.functions.contains_key(&resolved.mangled_name)
    {
        monomorphize_impl_method(
            c,
            &p.base_type,
            &p.method,
            &p.type_args,
            &p.method_type_args,
        )?;
    }

    let callee = *c
        .functions
        .get(&resolved.mangled_name)
        .ok_or_else(|| format!("undefined static function `{method}` on `{type_name}`"))?;

    Ok(ResolvedStaticCall {
        callee,
        mangled_name: resolved.mangled_name.as_str().to_string(),
        param_types: resolved.param_types,
        return_type: resolved.return_type,
    })
}

fn compile_static_call<'ctx>(
    c: &mut Compiler<'ctx>,
    type_name: &str,
    resolved_type: Option<&TypeIdentifier>,
    method: &str,
    args: &[Arg],
    function: FunctionValue<'ctx>,
) -> ExprResult<'ctx> {
    let resolved = resolve_static_call(c, type_name, resolved_type, method, args)?;

    let mut llvm_args: Vec<BasicMetadataValueEnum> = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        let val = if i < resolved.param_types.len() {
            compile_expr_coerced(c, &arg.value, &resolved.param_types[i], function)?
        } else {
            compile_expr(c, &arg.value, function)?.map(|tv| tv.value)
        }
        .ok_or_else(|| "static call argument produced no value".to_string())?;
        llvm_args.push(val.into());
    }

    Ok(c.call(
        resolved.callee,
        &llvm_args,
        &format!("{}_ret", resolved.mangled_name),
    )
    .map(|v| TypedValue::new(v, resolved.return_type)))
}
