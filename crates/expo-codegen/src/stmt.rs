//! Statement compilation seam.
//!
//! [`compile_statement`] is the codegen-side dispatcher for one
//! [`Statement`]. As of Phase 4g Slice 1 it is a thin shim that
//! lowers the statement into IR instructions + an optional
//! [`expo_ir::blocks::IRTerminator`] via [`expo_ir::Lowerer`] and
//! walks the result through the shared
//! [`crate::control::execute_instructions`] +
//! [`crate::control::emit_terminator`] dispatcher used by every
//! conditional construct's `emit_*` walker.
//!
//! ### Transitional fork: list-literal assignments
//!
//! Assignments whose RHS is an [`ExprKind::List`] literal still
//! route to [`compile_assignment`], the legacy AST-driven path.
//! The reason is the `from_list` protocol coercion (e.g.
//! `Set<Int> = [1, 2, 3]`): firing it from the LLVM-free Lowerer
//! would either require lowering to call back into codegen for
//! on-demand monomorphization of `target.from_list` (the opposite
//! of Phase 4g's "codegen consumes a closed `IRProgram`" goal) or
//! would require the executor to mangle and look up symbols by
//! string -- a code smell explicitly called out as the wrong
//! direction. The proper fix is the pre-codegen elaboration pass
//! arriving with the function-body lift in Slice 3; until then,
//! list-literal assignments stay on the legacy path. See
//! `expo_ir::lower::statements`'s module-level deferral note for
//! the architectural detail.
//!
//! Destructuring patterns also fall back to legacy because the
//! Lowerer rejects them today.

use expo_ast::ast::{AssignTarget, Expr, ExprKind, Pattern, Statement, TypeExpr};
use expo_ast::span::Span;
use expo_typecheck::context::Coercion;
use expo_typecheck::types::{Primitive, Type, mangle_name, mangle_type};
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use std::collections::HashMap;

use expo_ir::lower::inference::infer_type_from_expr as ir_infer_type_from_expr;
use expo_ir::lower::ownership::ownership_for_expr;
use expo_ir::lower::stmt::{
    resolve_annotation_subst, resolve_coercion, resolve_field_path, resolve_final_annotation_type,
    resolve_union_member,
};

use crate::compiler::Compiler;
use crate::expr::compile_expr;
use crate::generics::{ensure_types_exist, monomorphize_impl_method};
use crate::types::to_llvm_type;
use expo_ir::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier};

/// Compile one [`Statement`] by lowering it to IR and walking the
/// instructions + optional terminator through the shared
/// [`execute_instructions`] / [`emit_terminator`] dispatcher.
///
/// Two AST shapes still take the legacy `compile_assignment` path:
///
/// 1. `Statement::Assignment` whose RHS is an [`ExprKind::List`]
///    literal -- protocol-driven `from_list` coercion (e.g.
///    `Set<Int> = [1, 2, 3]`) is deferred to Slice 3 (see the
///    module-level note above and
///    `expo_ir::lower::statements`'s deferred-coercion section).
/// 2. `Statement::Assignment` against a destructuring
///    [`Pattern`] -- the Lowerer rejects them today; the legacy
///    path also rejects them, so this fork just preserves the
///    pre-existing diagnostic surface.
pub fn compile_statement<'ctx>(
    compiler: &mut Compiler<'ctx>,
    stmt: &Statement,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let span = statement_span(stmt);
    compiler.debug.set_location(
        compiler.context,
        &compiler.builder,
        span.start.line,
        span.start.column,
    );

    if needs_legacy_assignment_path(stmt) {
        let Statement::Assignment {
            target,
            type_annotation,
            value,
            ..
        } = stmt
        else {
            unreachable!("needs_legacy_assignment_path gated on Statement::Assignment");
        };
        compile_assignment(compiler, target, type_annotation, value, function)?;
        return Ok(None);
    }

    // Push annotation-derived `type_subst` entries (e.g. `T = Int` for
    // `list: List<Int> = ...`) before lowering AND executing the
    // statement. The IR Lowerer emits transitional
    // [`expo_ir::values::IRInstruction::Stub`] for expressions it
    // hasn't lifted yet; that Stub's deferred `compile_expr`
    // (resolving `List.new()` against the type-arg substitution
    // table) runs at execution time, so the subst entries must
    // outlive the lowering call.
    let saved_subst = push_assignment_annotation_subst(compiler, stmt);
    let result = lower_and_execute(compiler, stmt, function);
    if let Some(saved) = saved_subst {
        compiler.fn_lower.type_subst = saved;
    }
    result
}

/// Drive one statement through the new IR pipeline:
/// [`expo_ir::Lowerer::lower_statement`] -> [`execute_instructions`]
/// -> [`emit_terminator`]. Statements never reference cross-block
/// SSA so `value_map` starts empty; the only IR block ids a
/// statement-level terminator references are enclosing-construct ids
/// (e.g. a loop's `exit_block` referenced by `Statement::Break`).
/// Resolution falls through to
/// [`crate::compiler::FnState::block_table`] -- the fn-wide map every
/// per-construct emit walker registers its blocks into.
fn lower_and_execute<'ctx>(
    compiler: &mut Compiler<'ctx>,
    stmt: &Statement,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let mut builder = expo_ir::CFGBuilder::new();
    let entry = compiler.fn_lower.next_block_id();
    builder.add_block(entry, "stmt_entry");
    {
        let mut lowerer = compiler.lowerer();
        lowerer.lower_statement(&mut builder, entry, stmt)?;
    }
    let (blocks, closed) = builder.into_blocks_with_closed();
    crate::control::walk_function_blocks(compiler, &blocks, &closed, function, None)?;
    Ok(None)
}

/// Mirror of legacy `compile_assignment`'s annotation-subst push.
/// Returns the pre-push snapshot so the shim can restore it after the
/// IR pipeline runs. Returns `None` for non-assignment statements,
/// missing annotations, and annotations that resolve to no entries.
fn push_assignment_annotation_subst<'ctx>(
    compiler: &mut Compiler<'ctx>,
    stmt: &Statement,
) -> Option<HashMap<String, Type>> {
    let Statement::Assignment {
        type_annotation: Some(te),
        ..
    } = stmt
    else {
        return None;
    };
    let entries = resolve_annotation_subst(&compiler.lower_ctx(), te);
    if entries.is_empty() {
        return None;
    }
    let saved = compiler.fn_lower.type_subst.clone();
    for (name, ty) in entries {
        compiler.fn_lower.type_subst.insert(name, ty);
    }
    Some(saved)
}

/// Decide whether `stmt` requires the legacy `compile_assignment`
/// path. Three AST shapes route there today (Slice 1):
///
/// 1. `Statement::Assignment` whose RHS is an [`ExprKind::List`]
///    literal -- protocol-driven `from_list` coercion is deferred to
///    Slice 3 (see [`compile_statement`]'s doc-comment).
/// 2. `Statement::Assignment` against a destructuring [`Pattern`] --
///    the Lowerer rejects them today; the legacy path also rejects
///    them, so this fork preserves the pre-existing diagnostic.
/// 3. `Statement::Assignment` without a type annotation --
///    [`compile_expr`] computes the actual evaluated value type at
///    codegen time (e.g. `addrs = match Socket.resolve(...) ...`
///    settles `addrs` to `List<TCPAddr>`); the IR Lowerer can only
///    consult the typecheck-time `expr.resolved_type`, which is
///    `None` / `Type::Unknown` for many compound RHS shapes.
///    Annotated assignments are safe -- the annotation pins the
///    binding's type independent of RHS inference.
fn needs_legacy_assignment_path(stmt: &Statement) -> bool {
    let Statement::Assignment {
        target,
        value,
        type_annotation,
        ..
    } = stmt
    else {
        return false;
    };
    if matches!(value.kind, ExprKind::List { .. }) {
        return true;
    }
    if matches!(
        target,
        AssignTarget::Pattern(p) if !matches!(p, Pattern::Binding { .. })
    ) {
        return true;
    }
    type_annotation.is_none()
}

/// Compiles a let binding or reassignment, handling type annotations,
/// list literal conversions, and numeric coercions.
fn compile_assignment<'ctx>(
    compiler: &mut Compiler<'ctx>,
    target: &AssignTarget,
    type_annotation: &Option<TypeExpr>,
    value: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<(), String> {
    let mut saved_subst = None;
    if let Some(type_expression) = type_annotation {
        let entries = resolve_annotation_subst(&compiler.lower_ctx(), type_expression);
        if !entries.is_empty() {
            saved_subst = Some(compiler.fn_lower.type_subst.clone());
            for (name, resolved_type) in entries {
                compiler.fn_lower.type_subst.insert(name, resolved_type);
            }
        }
    }

    let val_tv =
        compile_expr(compiler, value, function)?.ok_or("assignment value produced no value")?;
    let raw_val = val_tv.value;
    let compiled_type = val_tv.expo_type;

    if let Some(saved) = saved_subst {
        compiler.fn_lower.type_subst = saved;
    }

    let assigned_type = if let Some(type_expression) = type_annotation {
        let annotated = resolve_final_annotation_type(&compiler.lower_ctx(), type_expression);
        let _ = ensure_types_exist(compiler, &annotated);
        annotated
    } else if compiled_type != Type::Unknown {
        compiled_type
    } else {
        infer_type_from_expr_codegen(compiler, value).unwrap_or(Type::Unknown)
    };

    let raw_val = if matches!(&value.kind, ExprKind::List { .. }) {
        convert_list_literal_if_needed(compiler, raw_val, &assigned_type)?
    } else {
        raw_val
    };

    let val = coerce_numeric(compiler, raw_val, &assigned_type);
    let val = apply_coercion(compiler, val, value)?;

    match target {
        AssignTarget::LValue(lvalue) => {
            if lvalue.segments.len() == 1 {
                let name = &lvalue.segments[0];
                if let Some((ptr, variable_type, _)) =
                    compiler.fn_state.variables.get(name).cloned()
                {
                    let store_val = coerce_numeric(compiler, val, &variable_type);
                    compiler.builder.build_store(ptr, store_val).unwrap();
                } else {
                    let ownership = ownership_for_expr(value, &assigned_type);
                    let alloca_type =
                        to_llvm_type(&assigned_type, compiler.context, &compiler.llvm_types)
                            .unwrap_or(val.get_type());
                    let alloca = compiler.build_entry_alloca(alloca_type, name);
                    compiler.builder.build_store(alloca, val).unwrap();
                    compiler
                        .fn_state
                        .variables
                        .insert(name.clone(), (alloca, assigned_type.clone(), ownership));
                    compiler
                        .fn_lower
                        .local_types
                        .insert(name.clone(), assigned_type);
                }
            } else {
                compile_field_assignment(compiler, &lvalue.segments, val)?;
            }
        }
        AssignTarget::Pattern(pat) => {
            let Pattern::Binding { name, .. } = pat else {
                return Err("destructuring patterns not yet supported in compilation".to_string());
            };

            let ownership = ownership_for_expr(value, &assigned_type);
            let alloca_type = to_llvm_type(&assigned_type, compiler.context, &compiler.llvm_types)
                .unwrap_or(val.get_type());
            let alloca = compiler.build_entry_alloca(alloca_type, name);
            compiler.builder.build_store(alloca, val).unwrap();
            compiler
                .fn_state
                .variables
                .insert(name.clone(), (alloca, assigned_type.clone(), ownership));
            compiler
                .fn_lower
                .local_types
                .insert(name.clone(), assigned_type);
        }
    }
    Ok(())
}

/// Emits the GEP chain for a dotted field path (`self.span.start.line`) and
/// returns the LLVM pointer to the final field plus its Expo type. This is
/// emission-only: the semantic decision (resolving each segment to a field
/// index and type) lives in [`expo_ir::lower::fields::resolve_field_path`];
/// this helper only walks the resulting steps and emits LLVM `getelementptr`
/// instructions.
fn field_ptr<'ctx>(
    compiler: &Compiler<'ctx>,
    segments: &[String],
) -> Result<(PointerValue<'ctx>, Type), String> {
    let (base_type, steps) = resolve_field_path(&compiler.lower_ctx(), segments, |name| {
        compiler
            .fn_state
            .variables
            .get(name)
            .map(|(_, ty, _)| ty.clone())
    })?;

    let variable_name = &segments[0];
    let (mut ptr, _, _) = compiler
        .fn_state
        .variables
        .get(variable_name)
        .unwrap()
        .clone();

    let mut current_type = base_type;
    for (i, step) in steps.iter().enumerate() {
        let struct_type = to_llvm_type(&current_type, compiler.context, &compiler.llvm_types)
            .map(|t| t.into_struct_type())
            .ok_or_else(|| format!("unknown struct type for field path segment {i}"))?;

        ptr = compiler
            .builder
            .build_struct_gep(
                struct_type,
                ptr,
                step.field_index,
                &format!("{variable_name}.{}", segments[i + 1]),
            )
            .unwrap();

        current_type = step.field_type.clone();
    }

    Ok((ptr, current_type))
}

/// Compiles a multi-segment field assignment like `self.x = value`.
fn compile_field_assignment<'ctx>(
    compiler: &mut Compiler<'ctx>,
    segments: &[String],
    val: BasicValueEnum<'ctx>,
) -> Result<(), String> {
    let (ptr, field_type) = field_ptr(compiler, segments)?;
    let store_val = coerce_numeric(compiler, val, &field_type);
    compiler.builder.build_store(ptr, store_val).unwrap();
    Ok(())
}

/// Extends, truncates, or no-ops integer and float LLVM values so they match
/// the target primitive type when storing or passing values.
pub(crate) fn coerce_numeric<'ctx>(
    compiler: &Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    target: &Type,
) -> BasicValueEnum<'ctx> {
    let Type::Primitive(target_prim) = target else {
        return val;
    };

    if val.is_int_value() && target_prim.is_integer() {
        let int_value = val.into_int_value();
        let src_bits = int_value.get_type().get_bit_width();
        let dst_bits = int_bit_width(target_prim);
        if src_bits == dst_bits {
            return int_value.into();
        }
        let dst_type = compiler.context.custom_width_int_type(dst_bits);
        if dst_bits < src_bits {
            return compiler
                .builder
                .build_int_truncate(int_value, dst_type, "trunc")
                .unwrap()
                .into();
        }
        let signed = matches!(
            target_prim,
            Primitive::I8 | Primitive::I16 | Primitive::I32 | Primitive::I64
        );
        if signed {
            compiler
                .builder
                .build_int_s_extend(int_value, dst_type, "sext")
                .unwrap()
                .into()
        } else {
            compiler
                .builder
                .build_int_z_extend(int_value, dst_type, "zext")
                .unwrap()
                .into()
        }
    } else if val.is_float_value() && target_prim.is_float() {
        let float_value = val.into_float_value();
        let dst_is_f64 = *target_prim == Primitive::F64;
        if (float_value.get_type() == compiler.context.f64_type()) == dst_is_f64 {
            return float_value.into();
        }
        if dst_is_f64 {
            compiler
                .builder
                .build_float_ext(float_value, compiler.context.f64_type(), "fpext")
                .unwrap()
                .into()
        } else {
            compiler
                .builder
                .build_float_trunc(float_value, compiler.context.f32_type(), "fptrunc")
                .unwrap()
                .into()
        }
    } else {
        val
    }
}

/// Wraps a concrete value into a tagged union representation.
pub(crate) fn compile_union_wrap<'ctx>(
    compiler: &mut Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    source: &Type,
    target_union: &Type,
) -> Result<BasicValueEnum<'ctx>, String> {
    if !matches!(target_union, Type::Union(_)) {
        return Ok(val);
    }

    let resolved = resolve_union_member(source, target_union)?;
    let union_type = compiler
        .llvm_types
        .get_monomorphized(&resolved.union_mangled)
        .ok_or_else(|| format!("union type {} not registered", resolved.union_mangled))?;

    let alloca = compiler
        .builder
        .build_alloca(union_type, "union_wrap")
        .unwrap();

    let tag_ptr = compiler
        .builder
        .build_struct_gep(union_type, alloca, 0, "tag_ptr")
        .unwrap();
    let tag_val = compiler.context.i8_type().const_int(resolved.tag, false);
    compiler.builder.build_store(tag_ptr, tag_val).unwrap();

    if union_type.count_fields() > 1 {
        let payload_ptr = compiler
            .builder
            .build_struct_gep(union_type, alloca, 1, "payload_ptr")
            .unwrap();
        compiler.builder.build_store(payload_ptr, val).unwrap();
    }

    let result = compiler
        .builder
        .build_load(union_type, alloca, "union_val")
        .unwrap();
    Ok(result)
}

/// Applies a recorded coercion to a compiled value, if one exists for the
/// given expression span. Currently handles union widening.
pub(crate) fn apply_coercion<'ctx>(
    compiler: &mut Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    expr: &Expr,
) -> Result<BasicValueEnum<'ctx>, String> {
    let span = expr_span(expr);
    let Some(coercion) = resolve_coercion(&compiler.lower_ctx(), span) else {
        return Ok(val);
    };
    match coercion {
        Coercion::UnionWiden { source, target } => {
            let target_mangled = mangle_type(&target);
            if let Some(target_llvm) = compiler
                .llvm_types
                .get_monomorphized(&MonomorphizedTypeIdentifier::new(&target_mangled))
                && val.get_type() == target_llvm.into()
            {
                return Ok(val);
            }
            compile_union_wrap(compiler, val, &source, &target)
        }
    }
}

/// Codegen-side wrapper around [`expo_ir::lower::inference::infer_type_from_expr`]
/// that bridges the IR helper to the LLVM-bound `Compiler.fn_state.variables`
/// map via a closure. The IR module is the canonical owner of the inference
/// logic (it lives in [`crate::lowerer`]); this wrapper exists only because
/// the legacy [`compile_assignment`] path still needs it during the Slice 1
/// transition.
fn infer_type_from_expr_codegen(c: &Compiler, expr: &Expr) -> Option<Type> {
    ir_infer_type_from_expr(
        &c.lower_ctx(),
        &|name: &str| c.fn_state.variables.get(name).map(|(_, ty, _)| ty.clone()),
        expr,
    )
}

/// When a list literal `[a, b, c]` is assigned to a non-List type that
/// implements `ListLiteral<T>` (e.g. `Set<T>`), calls `from_list` to convert.
fn convert_list_literal_if_needed<'ctx>(
    compiler: &mut Compiler<'ctx>,
    list_val: BasicValueEnum<'ctx>,
    target_type: &Type,
) -> Result<BasicValueEnum<'ctx>, String> {
    let (base, identifier, type_args) = match target_type {
        Type::Named {
            identifier,
            type_args,
        } if identifier.name != "List" && !type_args.is_empty() => (
            identifier.name.clone(),
            identifier.clone(),
            type_args.clone(),
        ),
        _ => return Ok(list_val),
    };

    let target_mangled = mangle_name(&identifier, &type_args);
    let from_list_fn_name = format!("{target_mangled}_from_list");
    if !compiler
        .functions
        .contains_key(&FunctionIdentifier::new(&from_list_fn_name))
    {
        monomorphize_impl_method(compiler, &base, "from_list", &type_args, &[])?;
    }
    let from_list_fn = *compiler
        .functions
        .get(&FunctionIdentifier::new(&from_list_fn_name))
        .ok_or_else(|| format!("{base} does not implement ListLiteral (no from_list)"))?;

    let result = compiler
        .call(from_list_fn, &[list_val.into()], "from_list")
        .ok_or("from_list returned void")?;

    Ok(result)
}

/// Returns the LLVM bit width for an integer primitive type.
fn int_bit_width(primitive: &Primitive) -> u32 {
    match primitive {
        Primitive::I8 | Primitive::U8 => 8,
        Primitive::I16 | Primitive::U16 => 16,
        Primitive::I32 | Primitive::U32 => 32,
        Primitive::I64 | Primitive::U64 => 64,
        _ => 0,
    }
}

/// Extracts the source span from a statement for debug location tracking.
fn statement_span(stmt: &Statement) -> Span {
    match stmt {
        Statement::Expr(expr) => expr_span(expr),
        Statement::Assignment { span, .. }
        | Statement::CompoundAssign { span, .. }
        | Statement::Return { span, .. }
        | Statement::Break { span, .. } => *span,
    }
}

/// Extracts the source span from an expression.
fn expr_span(expr: &Expr) -> Span {
    expr.span
}
