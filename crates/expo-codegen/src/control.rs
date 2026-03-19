//! Control flow compilation: if/else, cond, match, ternary, while loops,
//! and infinite loops with break support.

use crate::drop::Ownership;
use expo_ast::ast::{CondArm, Expr, FieldPattern, Literal, MatchArm, Pattern, Statement};
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{Type, mangle_type};
use inkwell::FloatPredicate;
use inkwell::IntPredicate;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use crate::compiler::Compiler;
use crate::expr::compile_expr;
use crate::stmt::compile_statement;
use crate::types::to_llvm_type;

/// Compiles a statement list and returns the value of the last expression.
/// Non-expression statements produce no value; only a trailing `Expr` is captured.
pub(crate) fn compile_body_as_value<'ctx>(
    c: &mut Compiler<'ctx>,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let mut val: Option<BasicValueEnum> = None;
    for (i, stmt) in body.iter().enumerate() {
        if c.current_block_terminated() {
            break;
        }
        if i == body.len() - 1
            && let Statement::Expr(expr) = stmt
        {
            val = compile_expr(c, expr, function)?;
            continue;
        }
        compile_statement(c, stmt, function)?;
    }
    Ok(val)
}

/// Compiles a `cond` expression (multi-arm conditional). Each arm's condition is
/// tested in order; the first truthy branch executes. Returns a phi value when
/// all arms (including `else`) produce a value of the same type.
pub fn compile_cond<'ctx>(
    c: &mut Compiler<'ctx>,
    arms: &[CondArm],
    else_body: &Option<Vec<Statement>>,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    if arms.is_empty() && else_body.is_none() {
        return Ok(None);
    }

    let merge_bb = c.context.append_basic_block(function, "cond_end");
    let fallthrough_bb = c.context.append_basic_block(function, "cond_none");
    let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
        Vec::new();

    for (i, arm) in arms.iter().enumerate() {
        let cond_val =
            compile_expr(c, &arm.condition, function)?.ok_or("cond arm produced no value")?;
        let cond_int = coerce_to_bool(c, cond_val, "cond arm condition")?;

        let body_bb = c
            .context
            .append_basic_block(function, &format!("cond_body_{i}"));
        let next_bb = if i + 1 < arms.len() {
            c.context
                .append_basic_block(function, &format!("cond_check_{}", i + 1))
        } else {
            fallthrough_bb
        };

        c.builder
            .build_conditional_branch(cond_int, body_bb, next_bb)
            .unwrap();

        c.builder.position_at_end(body_bb);
        let arm_val = compile_body_as_value(c, &arm.body, function)?;
        if !c.current_block_terminated() {
            c.builder.build_unconditional_branch(merge_bb).unwrap();
        }
        let arm_end_bb = c.builder.get_insert_block().unwrap();
        if let Some(val) = arm_val {
            incoming.push((val, arm_end_bb));
        }

        if next_bb != merge_bb && next_bb != fallthrough_bb {
            c.builder.position_at_end(next_bb);
        }
    }

    c.builder.position_at_end(fallthrough_bb);
    if let Some(body) = else_body {
        let else_val = compile_body_as_value(c, body, function)?;
        if !c.current_block_terminated() {
            c.builder.build_unconditional_branch(merge_bb).unwrap();
        }
        let else_end_bb = c.builder.get_insert_block().unwrap();
        if let Some(val) = else_val {
            incoming.push((val, else_end_bb));
        }
    } else {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }

    c.builder.position_at_end(merge_bb);

    let expected_sources = arms.len() + if else_body.is_some() { 1 } else { 0 };
    if !incoming.is_empty() && incoming.len() == expected_sources {
        let first_ty = incoming[0].0.get_type();
        if incoming.iter().all(|(v, _)| v.get_type() == first_ty) {
            let phi = c.builder.build_phi(first_ty, "condval").unwrap();
            for (v, bb) in &incoming {
                phi.add_incoming(&[(v, *bb)]);
            }
            return Ok(Some(phi.as_basic_value()));
        }
    }

    Ok(None)
}

/// Compiles an `if`/`else` expression. Returns a phi value when both branches
/// produce a value of the same type, otherwise returns `None`.
pub fn compile_if<'ctx>(
    c: &mut Compiler<'ctx>,
    condition: &Expr,
    then_body: &[Statement],
    else_body: &Option<Vec<Statement>>,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let cond_val = compile_expr(c, condition, function)?.ok_or("if condition produced no value")?;
    let cond_int = coerce_to_bool(c, cond_val, "if condition")?;

    let then_bb = c.context.append_basic_block(function, "then");
    let else_bb = c.context.append_basic_block(function, "else");
    let merge_bb = c.context.append_basic_block(function, "ifcont");

    c.builder
        .build_conditional_branch(cond_int, then_bb, else_bb)
        .unwrap();

    c.builder.position_at_end(then_bb);
    let then_val = compile_body_as_value(c, then_body, function)?;
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let then_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(else_bb);
    let else_val = if let Some(else_stmts) = else_body {
        compile_body_as_value(c, else_stmts, function)?
    } else {
        None
    };
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let else_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(merge_bb);

    if let (Some(tv), Some(ev)) = (&then_val, &else_val)
        && tv.get_type() == ev.get_type()
    {
        let phi = c.builder.build_phi(tv.get_type(), "ifval").unwrap();
        phi.add_incoming(&[(tv, then_end_bb), (ev, else_end_bb)]);
        return Ok(Some(phi.as_basic_value()));
    }

    Ok(None)
}

/// Compiles an `unless` guard: `unless cond ... end`. Negates the condition
/// and delegates to `compile_if` with no else branch.
pub fn compile_unless<'ctx>(
    c: &mut Compiler<'ctx>,
    condition: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let cond_val =
        compile_expr(c, condition, function)?.ok_or("unless condition produced no value")?;
    let cond_int = coerce_to_bool(c, cond_val, "unless condition")?;
    let negated = c.builder.build_not(cond_int, "unless_neg").unwrap();

    let then_bb = c.context.append_basic_block(function, "unless_body");
    let merge_bb = c.context.append_basic_block(function, "unless_end");

    c.builder
        .build_conditional_branch(negated, then_bb, merge_bb)
        .unwrap();

    c.builder.position_at_end(then_bb);
    for stmt in body {
        if c.current_block_terminated() {
            break;
        }
        compile_statement(c, stmt, function)?;
    }
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }

    c.builder.position_at_end(merge_bb);
    Ok(None)
}

/// Compiles an infinite `loop` block. Only exits via `break`.
pub fn compile_loop<'ctx>(
    c: &mut Compiler<'ctx>,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let loop_header = c.context.append_basic_block(function, "loop_header");
    let loop_body = c.context.append_basic_block(function, "loop_body");
    let loop_exit = c.context.append_basic_block(function, "loop_exit");

    c.builder.build_unconditional_branch(loop_header).unwrap();

    c.builder.position_at_end(loop_header);
    c.builder.build_unconditional_branch(loop_body).unwrap();

    c.builder.position_at_end(loop_body);
    c.loop_exit_stack.push(loop_exit);

    for stmt in body {
        if c.current_block_terminated() {
            break;
        }
        compile_statement(c, stmt, function)?;
    }

    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(loop_header).unwrap();
    }

    c.loop_exit_stack.pop();
    c.builder.position_at_end(loop_exit);

    Ok(None)
}

/// Compiles a `match` expression. Patterns are tested sequentially; the first
/// matching arm executes. Bindings introduced by patterns are scoped to their
/// arm. Returns a phi value when all arms produce a value of the same type.
pub fn compile_match<'ctx>(
    c: &mut Compiler<'ctx>,
    subject: &Expr,
    arms: &[MatchArm],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let subject_val =
        compile_expr(c, subject, function)?.ok_or("match subject produced no value")?;

    let subject_type = infer_subject_type(c, subject, &subject_val);

    let subject_alloca = c
        .builder
        .build_alloca(subject_val.get_type(), "match_subject")
        .unwrap();
    c.builder.build_store(subject_alloca, subject_val).unwrap();

    let merge_bb = c.context.append_basic_block(function, "match_end");
    let fallthrough_bb = c.context.append_basic_block(function, "match_none");
    let mut incoming: Vec<(BasicValueEnum<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
        Vec::new();

    let mut reachable_arm_count = 0usize;

    for (i, arm) in arms.iter().enumerate() {
        let body_bb = c
            .context
            .append_basic_block(function, &format!("match_body_{i}"));
        let next_bb = if i + 1 < arms.len() {
            c.context
                .append_basic_block(function, &format!("match_test_{}", i + 1))
        } else {
            fallthrough_bb
        };

        let saved_vars = c.variables.clone();

        let condition = compile_pattern(c, &arm.pattern, subject_alloca, &subject_type, function)?;

        let final_cond = if let Some(guard) = &arm.guard {
            let guard_val =
                compile_expr(c, guard, function)?.ok_or("match guard produced no value")?;
            c.builder
                .build_and(condition, guard_val.into_int_value(), "guard_and")
                .unwrap()
        } else {
            condition
        };

        c.builder
            .build_conditional_branch(final_cond, body_bb, next_bb)
            .unwrap();

        c.builder.position_at_end(body_bb);
        let arm_val = compile_body_as_value(c, &arm.body, function)?;
        let arm_terminated = c.current_block_terminated();
        if !arm_terminated {
            c.builder.build_unconditional_branch(merge_bb).unwrap();
            reachable_arm_count += 1;
        }
        let arm_end_bb = c.builder.get_insert_block().unwrap();
        if let Some(val) = arm_val {
            incoming.push((val, arm_end_bb));
        }

        c.variables = saved_vars;
        c.builder.position_at_end(next_bb);
    }

    c.builder.position_at_end(fallthrough_bb);
    c.builder.build_unconditional_branch(merge_bb).unwrap();

    c.builder.position_at_end(merge_bb);

    if !incoming.is_empty() && incoming.len() == reachable_arm_count {
        let first_ty = incoming[0].0.get_type();
        if incoming.iter().all(|(v, _)| v.get_type() == first_ty) {
            let undef = first_ty.const_zero();
            incoming.push((undef, fallthrough_bb));

            let phi = c.builder.build_phi(first_ty, "matchval").unwrap();
            for (v, bb) in &incoming {
                phi.add_incoming(&[(v, *bb)]);
            }
            return Ok(Some(phi.as_basic_value()));
        }
    }

    Ok(None)
}

/// Compiles a ternary expression (`condition ? then_expr : else_expr`).
/// Always value-producing when both branches yield the same type.
pub fn compile_ternary<'ctx>(
    c: &mut Compiler<'ctx>,
    condition: &Expr,
    then_expr: &Expr,
    else_expr: &Expr,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let cond_val =
        compile_expr(c, condition, function)?.ok_or("ternary condition produced no value")?;
    let cond_int = coerce_to_bool(c, cond_val, "ternary condition")?;

    let then_bb = c.context.append_basic_block(function, "tern_then");
    let else_bb = c.context.append_basic_block(function, "tern_else");
    let merge_bb = c.context.append_basic_block(function, "tern_cont");

    c.builder
        .build_conditional_branch(cond_int, then_bb, else_bb)
        .unwrap();

    c.builder.position_at_end(then_bb);
    let then_val = compile_expr(c, then_expr, function)?;
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let then_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(else_bb);
    let else_val = compile_expr(c, else_expr, function)?;
    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(merge_bb).unwrap();
    }
    let else_end_bb = c.builder.get_insert_block().unwrap();

    c.builder.position_at_end(merge_bb);

    if let (Some(tv), Some(ev)) = (&then_val, &else_val)
        && tv.get_type() == ev.get_type()
    {
        let phi = c.builder.build_phi(tv.get_type(), "ternval").unwrap();
        phi.add_incoming(&[(tv, then_end_bb), (ev, else_end_bb)]);
        return Ok(Some(phi.as_basic_value()));
    }

    Ok(None)
}

/// Compiles a `while` loop. Condition is re-evaluated each iteration.
pub fn compile_while<'ctx>(
    c: &mut Compiler<'ctx>,
    condition: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let while_header = c.context.append_basic_block(function, "while_header");
    let while_body = c.context.append_basic_block(function, "while_body");
    let while_exit = c.context.append_basic_block(function, "while_exit");

    c.builder.build_unconditional_branch(while_header).unwrap();

    c.builder.position_at_end(while_header);
    let cond_val =
        compile_expr(c, condition, function)?.ok_or("while condition produced no value")?;
    let cond_int = coerce_to_bool(c, cond_val, "while condition")?;
    c.builder
        .build_conditional_branch(cond_int, while_body, while_exit)
        .unwrap();

    c.builder.position_at_end(while_body);
    c.loop_exit_stack.push(while_exit);

    for stmt in body {
        if c.current_block_terminated() {
            break;
        }
        compile_statement(c, stmt, function)?;
    }

    if !c.current_block_terminated() {
        c.builder.build_unconditional_branch(while_header).unwrap();
    }

    c.loop_exit_stack.pop();
    c.builder.position_at_end(while_exit);

    Ok(None)
}

/// Compiles a `for` loop by desugaring into an indexed while loop:
///   idx = 0; len = iterable.length(); while idx < len { elem = iterable.get(idx); body; idx += 1 }
pub fn compile_for<'ctx>(
    c: &mut Compiler<'ctx>,
    pattern: &expo_ast::ast::Pattern,
    iterable: &Expr,
    body: &[Statement],
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    let iter_val = compile_expr(c, iterable, function)?.ok_or("for iterable produced no value")?;

    let iter_ty = crate::stmt::infer_type_from_llvm(c, &iter_val);
    let iter_llvm_ty = iter_val.get_type();

    let iter_alloca = c.builder.build_alloca(iter_llvm_ty, "for_iter").unwrap();
    c.builder.build_store(iter_alloca, iter_val).unwrap();
    c.variables.insert(
        "__for_iter".to_string(),
        (iter_alloca, iter_ty.clone(), Ownership::Unowned),
    );

    let (mangled_type, elem_llvm_ty, elem_expo_ty, base, type_args) =
        resolve_enumerable_info(c, &iter_ty)?;

    c.monomorphize_impl_method(&base, "length", &type_args)?;
    c.monomorphize_impl_method(&base, "get", &type_args)?;

    let length_fn_name = format!("{}_length", mangled_type);
    let get_fn_name = format!("{}_get", mangled_type);

    let length_fn = *c
        .functions
        .get(&length_fn_name)
        .ok_or_else(|| format!("no function `{length_fn_name}`"))?;
    let get_fn = *c
        .functions
        .get(&get_fn_name)
        .ok_or_else(|| format!("no function `{get_fn_name}`"))?;

    let i64_ty = c.context.i64_type();

    let iter_loaded = c
        .builder
        .build_load(iter_llvm_ty, iter_alloca, "iter_load")
        .unwrap();
    let len_val = c
        .builder
        .build_call(length_fn, &[iter_loaded.into()], "len")
        .unwrap()
        .try_as_basic_value()
        .left()
        .ok_or("length() returned void")?
        .into_int_value();

    let idx_alloca = c.builder.build_alloca(i64_ty, "for_idx").unwrap();
    c.builder
        .build_store(idx_alloca, i64_ty.const_int(0, false))
        .unwrap();

    let header_bb = c.context.append_basic_block(function, "for_header");
    let body_bb = c.context.append_basic_block(function, "for_body");
    let exit_bb = c.context.append_basic_block(function, "for_exit");

    c.builder.build_unconditional_branch(header_bb).unwrap();

    c.builder.position_at_end(header_bb);
    let idx = c
        .builder
        .build_load(i64_ty, idx_alloca, "idx")
        .unwrap()
        .into_int_value();
    let cond = c
        .builder
        .build_int_compare(IntPredicate::ULT, idx, len_val, "for_cond")
        .unwrap();
    c.builder
        .build_conditional_branch(cond, body_bb, exit_bb)
        .unwrap();

    c.builder.position_at_end(body_bb);
    c.loop_exit_stack.push(exit_bb);

    let iter_for_get = c
        .builder
        .build_load(iter_llvm_ty, iter_alloca, "iter_get")
        .unwrap();
    let idx_for_get = c.builder.build_load(i64_ty, idx_alloca, "idx_get").unwrap();
    let elem_val = c
        .builder
        .build_call(get_fn, &[iter_for_get.into(), idx_for_get.into()], "elem")
        .unwrap()
        .try_as_basic_value()
        .left()
        .ok_or("get() returned void")?;

    if let expo_ast::ast::Pattern::Binding { name, .. } = pattern {
        let alloca = c.builder.build_alloca(elem_llvm_ty, name).unwrap();
        c.builder.build_store(alloca, elem_val).unwrap();
        c.variables.insert(
            name.clone(),
            (alloca, elem_expo_ty.clone(), Ownership::Unowned),
        );
    }

    for stmt in body {
        if c.current_block_terminated() {
            break;
        }
        compile_statement(c, stmt, function)?;
    }

    if !c.current_block_terminated() {
        let cur_idx = c
            .builder
            .build_load(i64_ty, idx_alloca, "cur_idx")
            .unwrap()
            .into_int_value();
        let next_idx = c
            .builder
            .build_int_add(cur_idx, i64_ty.const_int(1, false), "next_idx")
            .unwrap();
        c.builder.build_store(idx_alloca, next_idx).unwrap();
        c.builder.build_unconditional_branch(header_bb).unwrap();
    }

    c.loop_exit_stack.pop();
    c.variables.remove("__for_iter");
    c.builder.position_at_end(exit_bb);

    Ok(None)
}

/// Resolves the mangled name, element LLVM type, element Expo type, base name,
/// and type args for any type that implements the `Enumeration` protocol.
fn resolve_enumerable_info<'ctx>(
    c: &Compiler<'ctx>,
    ty: &Type,
) -> Result<
    (
        String,
        inkwell::types::BasicTypeEnum<'ctx>,
        Type,
        String,
        Vec<Type>,
    ),
    String,
> {
    let (base, type_args) = match ty {
        Type::GenericInstance {
            base, type_args, ..
        } => (base.clone(), type_args.clone()),
        Type::Struct(name) => {
            if let Some((base, type_args)) = crate::generics::try_parse_mangled_name(name, c) {
                (base, type_args)
            } else {
                return Err(format!(
                    "`for` requires an Enumeration type, found `{}`",
                    ty.display()
                ));
            }
        }
        _ => {
            return Err(format!(
                "`for` requires an Enumeration type, found `{}`",
                ty.display()
            ));
        }
    };

    let protos = c
        .type_ctx
        .protocol_impls
        .get(&base)
        .ok_or_else(|| format!("`{}` does not implement the Enumeration protocol", base))?;
    if !protos.iter().any(|p| p == "Enumeration") {
        return Err(format!(
            "`{}` does not implement the Enumeration protocol",
            base
        ));
    }

    let struct_info = c
        .type_ctx
        .structs
        .get(&base)
        .ok_or_else(|| format!("no struct info for `{base}`"))?;
    let get_sig = struct_info
        .methods
        .get("get")
        .ok_or_else(|| format!("`{base}` implements Enumeration but has no `get` method"))?;
    let subst = expo_typecheck::types::build_substitution(&struct_info.type_params, &type_args);
    let elem_expo_ty = expo_typecheck::types::substitute(&get_sig.return_type, &subst);

    let elem_llvm = to_llvm_type(&elem_expo_ty, c.context, &c.struct_types)
        .ok_or("cannot resolve element LLVM type")?;
    let mangled = expo_typecheck::types::mangle_name(&base, &type_args);

    Ok((mangled, elem_llvm, elem_expo_ty, base, type_args))
}

/// Converts an integer value to a 1-bit bool. Already-boolean values pass
/// through; wider ints are compared != 0.
fn coerce_to_bool<'ctx>(
    c: &Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    label: &str,
) -> Result<IntValue<'ctx>, String> {
    if !val.is_int_value() {
        return Err(format!("{label} must be a boolean"));
    }

    let iv = val.into_int_value();
    if iv.get_type().get_bit_width() == 1 {
        Ok(iv)
    } else {
        Ok(c.builder
            .build_int_compare(IntPredicate::NE, iv, iv.get_type().const_zero(), label)
            .unwrap())
    }
}

/// Reconstructs the Expo type from an LLVM value, since LLVM IR discards
/// source-level type information. Checks variable bindings first, then
/// falls back to inspecting the LLVM type.
fn infer_subject_type<'ctx>(
    c: &Compiler<'ctx>,
    subject: &Expr,
    val: &BasicValueEnum<'ctx>,
) -> Type {
    if let Expr::Ident { name, .. } = subject
        && let Some((_, ty, _)) = c.variables.get(name)
    {
        return ty.clone();
    }
    if matches!(subject, Expr::Self_ { .. })
        && let Some((_, ty, _)) = c.variables.get("self")
    {
        return ty.clone();
    }
    if val.is_int_value() {
        match val.into_int_value().get_type().get_bit_width() {
            1 => Type::Primitive(expo_typecheck::types::Primitive::Bool),
            32 => Type::Primitive(expo_typecheck::types::Primitive::I32),
            64 => Type::Primitive(expo_typecheck::types::Primitive::I64),
            _ => Type::Unknown,
        }
    } else if val.is_struct_value() {
        let st = val.into_struct_value().get_type();
        if let Some(name) = st.get_name() {
            let name_str = name.to_str().unwrap_or("");
            if c.type_ctx.enums.contains_key(name_str) {
                return Type::Enum(name_str.to_string());
            }
            if c.type_ctx.structs.contains_key(name_str) {
                return Type::Struct(name_str.to_string());
            }
            for ty in c.type_ctx.type_aliases.values() {
                if let Type::Union(_) = ty
                    && mangle_type(ty) == name_str
                {
                    return ty.clone();
                }
            }
        }
        Type::Unknown
    } else {
        Type::Unknown
    }
}

/// Recursively compiles a match pattern into a boolean condition. As a side
/// effect, binds matched variables into the compiler's variable scope.
fn compile_pattern<'ctx>(
    c: &mut Compiler<'ctx>,
    pattern: &Pattern,
    subject_ptr: PointerValue<'ctx>,
    subject_type: &Type,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let true_val = c.context.bool_type().const_int(1, false);

    match pattern {
        Pattern::Wildcard { .. } => Ok(true_val),

        Pattern::Binding { name, .. } => {
            let llvm_ty = to_llvm_type(subject_type, c.context, &c.struct_types)
                .ok_or("cannot load subject of unsupported type in pattern")?;
            let val = c.builder.build_load(llvm_ty, subject_ptr, name).unwrap();
            let alloca = c.builder.build_alloca(llvm_ty, name).unwrap();
            c.builder.build_store(alloca, val).unwrap();
            c.variables.insert(
                name.clone(),
                (alloca, subject_type.clone(), Ownership::Unowned),
            );
            Ok(true_val)
        }

        Pattern::Literal { value, .. } => {
            let llvm_ty = to_llvm_type(subject_type, c.context, &c.struct_types)
                .ok_or("cannot load subject for literal comparison")?;
            let subject_val = c
                .builder
                .build_load(llvm_ty, subject_ptr, "lit_subj")
                .unwrap();
            let lit_val = compile_literal_for_pattern(c, value)?;
            match_values(c, &subject_val, &lit_val)
        }

        Pattern::EnumUnit {
            type_path, variant, ..
        } => {
            let enum_name = enum_name_from_path(type_path, subject_type)?;
            compile_tag_check(c, subject_ptr, &enum_name, variant)
        }

        Pattern::EnumTuple {
            type_path,
            variant,
            elements,
            ..
        } => {
            let enum_name = enum_name_from_path(type_path, subject_type)?;
            let mut result = compile_tag_check(c, subject_ptr, &enum_name, variant)?;
            let (payload_type, payload_ptr) = get_payload_ptr(c, subject_ptr, &enum_name, variant)?;
            let field_types = get_tuple_variant_types(c, &enum_name, variant)?;
            result = compile_tuple_elements(
                c,
                elements,
                &field_types,
                payload_type,
                payload_ptr,
                result,
                function,
            )?;
            Ok(result)
        }

        Pattern::EnumStruct {
            type_path,
            variant,
            fields,
            ..
        } => {
            let enum_name = enum_name_from_path(type_path, subject_type)?;
            let mut result = compile_tag_check(c, subject_ptr, &enum_name, variant)?;
            let (payload_type, payload_ptr) = get_payload_ptr(c, subject_ptr, &enum_name, variant)?;
            let expected_fields = get_struct_variant_fields(c, &enum_name, variant)?;

            for fp in fields {
                result = compile_field_pattern(
                    c,
                    fp,
                    &expected_fields,
                    payload_type,
                    payload_ptr,
                    result,
                    &enum_name,
                    variant,
                    function,
                )?;
            }

            Ok(result)
        }

        Pattern::Constructor { name, elements, .. } => {
            let enum_name = find_constructor_enum(c, name, subject_type)?;
            let mut result = compile_tag_check(c, subject_ptr, &enum_name, name)?;

            if !elements.is_empty() {
                let (payload_type, payload_ptr) =
                    get_payload_ptr(c, subject_ptr, &enum_name, name)?;
                let field_types = get_tuple_variant_types(c, &enum_name, name)?;
                result = compile_tuple_elements(
                    c,
                    elements,
                    &field_types,
                    payload_type,
                    payload_ptr,
                    result,
                    function,
                )?;
            }

            Ok(result)
        }

        Pattern::Tuple { .. } | Pattern::List { .. } => {
            Err("tuple and list patterns not yet supported in compilation".to_string())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_field_pattern<'ctx>(
    c: &mut Compiler<'ctx>,
    fp: &FieldPattern,
    expected_fields: &[(String, Type)],
    payload_type: inkwell::types::StructType<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    mut result: IntValue<'ctx>,
    enum_name: &str,
    variant: &str,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    let (field_idx, (_, field_type)) = expected_fields
        .iter()
        .enumerate()
        .find(|(_, (name, _))| *name == fp.name)
        .ok_or_else(|| format!("unknown field `{}` in {enum_name}.{variant}", fp.name))?;

    let field_llvm_ty = to_llvm_type(field_type, c.context, &c.struct_types)
        .ok_or_else(|| format!("unsupported field type for `{}`", fp.name))?;
    let field_ptr = c
        .builder
        .build_struct_gep(payload_type, payload_ptr, field_idx as u32, &fp.name)
        .unwrap();
    let field_val = c
        .builder
        .build_load(field_llvm_ty, field_ptr, &format!("{}_val", fp.name))
        .unwrap();
    let field_alloca = c
        .builder
        .build_alloca(field_llvm_ty, &format!("{}_tmp", fp.name))
        .unwrap();
    c.builder.build_store(field_alloca, field_val).unwrap();

    if let Some(sub_pat) = &fp.pattern {
        let sub_result = compile_pattern(c, sub_pat, field_alloca, field_type, function)?;
        result = c
            .builder
            .build_and(result, sub_result, &format!("{}_and", fp.name))
            .unwrap();
    } else {
        c.variables.insert(
            fp.name.clone(),
            (field_alloca, field_type.clone(), Ownership::Unowned),
        );
    }

    Ok(result)
}

fn compile_literal_for_pattern<'ctx>(
    c: &Compiler<'ctx>,
    lit: &Literal,
) -> Result<BasicValueEnum<'ctx>, String> {
    match lit {
        Literal::Int(s) => {
            let val = crate::util::parse_int_literal(s)?;
            Ok(c.context.i64_type().const_int(val as u64, true).into())
        }
        Literal::Float(s) => {
            let val: f64 = s.parse().map_err(|_| format!("invalid float: {s}"))?;
            Ok(c.context.f64_type().const_float(val).into())
        }
        Literal::Bool(b) => Ok(c
            .context
            .bool_type()
            .const_int(if *b { 1 } else { 0 }, false)
            .into()),
        _ => Err("unsupported literal in match pattern".to_string()),
    }
}

fn compile_tag_check<'ctx>(
    c: &mut Compiler<'ctx>,
    subject_ptr: PointerValue<'ctx>,
    enum_name: &str,
    variant: &str,
) -> Result<IntValue<'ctx>, String> {
    let enum_type = *c
        .struct_types
        .get(enum_name)
        .ok_or_else(|| format!("unknown enum: {enum_name}"))?;
    let tag = c
        .get_variant_tag(enum_name, variant)
        .ok_or_else(|| format!("unknown variant: {enum_name}.{variant}"))?;
    let tag_ptr = c
        .builder
        .build_struct_gep(enum_type, subject_ptr, 0, "tag_ptr")
        .unwrap();
    let tag_val = c
        .builder
        .build_load(c.context.i8_type(), tag_ptr, "tag")
        .unwrap()
        .into_int_value();
    let expected = c.context.i8_type().const_int(tag as u64, false);
    Ok(c.builder
        .build_int_compare(IntPredicate::EQ, tag_val, expected, "tag_eq")
        .unwrap())
}

fn compile_tuple_elements<'ctx>(
    c: &mut Compiler<'ctx>,
    elements: &[Pattern],
    field_types: &[Type],
    payload_type: inkwell::types::StructType<'ctx>,
    payload_ptr: PointerValue<'ctx>,
    mut result: IntValue<'ctx>,
    function: FunctionValue<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    for (i, sub_pat) in elements.iter().enumerate() {
        let field_type = &field_types[i];
        let field_llvm_ty = to_llvm_type(field_type, c.context, &c.struct_types)
            .ok_or("unsupported field type in enum variant")?;
        let field_ptr = c
            .builder
            .build_struct_gep(payload_type, payload_ptr, i as u32, &format!("tp{i}"))
            .unwrap();
        let field_val = c
            .builder
            .build_load(field_llvm_ty, field_ptr, &format!("tp{i}_val"))
            .unwrap();
        let field_alloca = c
            .builder
            .build_alloca(field_llvm_ty, &format!("tp{i}_tmp"))
            .unwrap();
        c.builder.build_store(field_alloca, field_val).unwrap();

        let sub_result = compile_pattern(c, sub_pat, field_alloca, field_type, function)?;
        result = c
            .builder
            .build_and(result, sub_result, &format!("tp{i}_and"))
            .unwrap();
    }
    Ok(result)
}

fn enum_name_from_path(type_path: &[String], subject_type: &Type) -> Result<String, String> {
    match subject_type {
        Type::GenericInstance {
            base,
            type_args,
            kind: expo_typecheck::types::GenericKind::Enum,
            ..
        } => Ok(expo_typecheck::types::mangle_name(base, type_args)),
        Type::Enum(name) => Ok(name.clone()),
        Type::Union(_) => Ok(mangle_type(subject_type)),
        _ if !type_path.is_empty() => Ok(type_path.join(".")),
        _ => Err("cannot determine enum name for pattern".to_string()),
    }
}

fn find_constructor_enum<'ctx>(
    c: &Compiler<'ctx>,
    variant_name: &str,
    subject_type: &Type,
) -> Result<String, String> {
    if let Type::GenericInstance {
        base,
        type_args,
        kind: expo_typecheck::types::GenericKind::Enum,
        ..
    } = subject_type
    {
        return Ok(expo_typecheck::types::mangle_name(base, type_args));
    }
    if let Type::Enum(name) = subject_type {
        return Ok(name.clone());
    }
    if let Type::Union(members) = subject_type {
        let member_mangled = mangle_type(&Type::Struct(variant_name.to_string()));
        if members.iter().any(|m| mangle_type(m) == member_mangled) {
            return Ok(mangle_type(subject_type));
        }
    }
    for (enum_name, info) in &c.type_ctx.enums {
        if info.variants.iter().any(|v| v.name == variant_name) {
            return Ok(enum_name.clone());
        }
    }
    Err(format!("no enum found with variant `{variant_name}`"))
}

fn get_payload_ptr<'ctx>(
    c: &mut Compiler<'ctx>,
    subject_ptr: PointerValue<'ctx>,
    enum_name: &str,
    variant: &str,
) -> Result<(inkwell::types::StructType<'ctx>, PointerValue<'ctx>), String> {
    let payload_type = c
        .get_variant_payload_type(enum_name, variant)
        .ok_or_else(|| format!("no payload type for {enum_name}.{variant}"))?;
    let enum_type = *c
        .struct_types
        .get(enum_name)
        .ok_or_else(|| format!("unknown enum: {enum_name}"))?;
    let payload_ptr = c
        .builder
        .build_struct_gep(enum_type, subject_ptr, 1, "payload_ptr")
        .unwrap();
    Ok((payload_type, payload_ptr))
}

fn get_struct_variant_fields(
    c: &Compiler<'_>,
    enum_name: &str,
    variant: &str,
) -> Result<Vec<(String, Type)>, String> {
    let data = lookup_variant_data(c, enum_name, variant)?;
    match data {
        VariantData::Struct(fields) => Ok(fields),
        _ => Err(format!("{enum_name}.{variant} is not a struct variant")),
    }
}

fn get_tuple_variant_types(
    c: &Compiler<'_>,
    enum_name: &str,
    variant: &str,
) -> Result<Vec<Type>, String> {
    let data = lookup_variant_data(c, enum_name, variant)?;
    match data {
        VariantData::Tuple(types) => Ok(types),
        _ => Err(format!("{enum_name}.{variant} is not a tuple variant")),
    }
}

fn lookup_variant_data(
    c: &Compiler<'_>,
    enum_name: &str,
    variant: &str,
) -> Result<VariantData, String> {
    if let Some(ei) = c.type_ctx.enums.get(enum_name)
        && let Some(vi) = ei.variants.iter().find(|v| v.name == variant)
    {
        return Ok(vi.data.clone());
    }
    if let Some(variants) = c.mono_enum_variants.get(enum_name)
        && let Some((_, data)) = variants.iter().find(|(n, _)| n == variant)
    {
        return Ok(data.clone());
    }
    Err(format!("variant not found: {enum_name}.{variant}"))
}

fn match_values<'ctx>(
    c: &Compiler<'ctx>,
    subject: &BasicValueEnum<'ctx>,
    lit: &BasicValueEnum<'ctx>,
) -> Result<IntValue<'ctx>, String> {
    if subject.is_int_value() && lit.is_int_value() {
        let subj_iv = subject.into_int_value();
        let mut lit_iv = lit.into_int_value();
        let subj_bits = subj_iv.get_type().get_bit_width();
        let lit_bits = lit_iv.get_type().get_bit_width();
        if subj_bits != lit_bits {
            let target_ty = c.context.custom_width_int_type(subj_bits);
            lit_iv = if subj_bits < lit_bits {
                c.builder
                    .build_int_truncate(lit_iv, target_ty, "lit_trunc")
                    .unwrap()
            } else {
                c.builder
                    .build_int_s_extend(lit_iv, target_ty, "lit_sext")
                    .unwrap()
            };
        }
        Ok(c.builder
            .build_int_compare(IntPredicate::EQ, subj_iv, lit_iv, "lit_eq")
            .unwrap())
    } else if subject.is_float_value() && lit.is_float_value() {
        Ok(c.builder
            .build_float_compare(
                FloatPredicate::OEQ,
                subject.into_float_value(),
                lit.into_float_value(),
                "lit_feq",
            )
            .unwrap())
    } else {
        Err("unsupported literal pattern comparison".to_string())
    }
}
