//! Statement compilation: let bindings, assignments, compound assignments,
//! return, break, and expression statements.

use expo_ast::ast::{AssignTarget, ClosureParam, Expr, Statement};
use expo_typecheck::context::Coercion;
use expo_typecheck::types::{GenericKind, Primitive, Type, mangle_name, mangle_type};
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::compiler::Compiler;
use crate::drop::Ownership;
use crate::expr::compile_expr;
use crate::structs::infer_static_method_return_type;
use crate::types::to_llvm_type;

/// Compiles a single statement (assignment, return, break, or compound
/// assignment). Expression statements are compiled for side effects only.
pub fn compile_statement<'ctx>(
    c: &mut Compiler<'ctx>,
    stmt: &Statement,
    function: FunctionValue<'ctx>,
) -> Result<Option<BasicValueEnum<'ctx>>, String> {
    match stmt {
        Statement::Expr(expr) => {
            compile_expr(c, expr, function)?;
            Ok(None)
        }

        Statement::Assignment {
            target,
            type_annotation,
            value,
            ..
        } => {
            let mut saved_subst = None;
            if let Some(te) = type_annotation {
                let annotated = c.resolve_type_expr(te);
                if let Type::GenericInstance {
                    base, type_args, ..
                } = &annotated
                {
                    let type_params = c
                        .type_ctx
                        .structs
                        .get(base.as_str())
                        .map(|si| si.type_params.clone())
                        .or_else(|| {
                            c.type_ctx
                                .enums
                                .get(base.as_str())
                                .map(|ei| ei.type_params.clone())
                        });
                    if let Some(tp) = type_params {
                        saved_subst = Some(c.type_subst.clone());
                        for (param, arg) in tp.iter().zip(type_args.iter()) {
                            let concrete = expo_typecheck::types::substitute(arg, &c.type_subst);
                            c.type_subst.insert(param.clone(), concrete);
                        }
                    }
                }
            }

            let raw_val =
                compile_expr(c, value, function)?.ok_or("assignment value produced no value")?;

            if let Some(saved) = saved_subst {
                c.type_subst = saved;
            }

            let ty = if let Some(te) = type_annotation {
                let annotated = c.resolve_type_expr(te);
                let annotated = match annotated {
                    Type::GenericInstance {
                        base,
                        kind,
                        type_args,
                    } => {
                        let resolved_args: Vec<Type> = type_args
                            .iter()
                            .map(|t| expo_typecheck::types::substitute_preserving(t, &c.type_subst))
                            .collect();
                        Type::GenericInstance {
                            base,
                            kind,
                            type_args: resolved_args,
                        }
                    }
                    other => other,
                };
                let _ = c.ensure_types_exist(&annotated);
                annotated
            } else {
                infer_type_from_expr(c, value).unwrap_or_else(|| infer_type_from_llvm(c, &raw_val))
            };

            let raw_val = if matches!(value, expo_ast::ast::Expr::List { .. }) {
                convert_list_literal_if_needed(c, raw_val, &ty)?
            } else {
                raw_val
            };

            let val = coerce_numeric(c, raw_val, &ty);
            let val = apply_coercion(c, val, value)?;

            match target {
                AssignTarget::LValue(lvalue) => {
                    if lvalue.segments.len() == 1 {
                        let name = &lvalue.segments[0];
                        if let Some((ptr, var_ty, _)) = c.variables.get(name).cloned() {
                            let store_val = coerce_numeric(c, val, &var_ty);
                            c.builder.build_store(ptr, store_val).unwrap();
                        } else {
                            let ownership = ownership_for_expr(value, &ty);
                            let alloca_ty = to_llvm_type(&ty, c.context, &c.struct_types)
                                .unwrap_or(val.get_type());
                            let alloca = c.builder.build_alloca(alloca_ty, name).unwrap();
                            c.builder.build_store(alloca, val).unwrap();
                            c.variables.insert(name.clone(), (alloca, ty, ownership));
                        }
                    } else {
                        compile_field_assignment(c, &lvalue.segments, val)?;
                    }
                }
                AssignTarget::Pattern(pat) => {
                    let expo_ast::ast::Pattern::Binding { name, .. } = pat else {
                        return Err(
                            "destructuring patterns not yet supported in compilation".to_string()
                        );
                    };

                    let ownership = ownership_for_expr(value, &ty);
                    let alloca_ty =
                        to_llvm_type(&ty, c.context, &c.struct_types).unwrap_or(val.get_type());
                    let alloca = c.builder.build_alloca(alloca_ty, name).unwrap();
                    c.builder.build_store(alloca, val).unwrap();
                    c.variables.insert(name.clone(), (alloca, ty, ownership));
                }
            }
            Ok(None)
        }

        Statement::Return { value, .. } => {
            crate::drop::drop_live_variables(c);
            if let Some(expr) = value {
                let val = compile_expr(c, expr, function)?;
                if let Some(v) = val {
                    let v = apply_coercion(c, v, expr)?;
                    c.builder.build_return(Some(&v)).unwrap();
                } else {
                    c.builder.build_return(None).unwrap();
                }
            } else {
                c.builder.build_return(None).unwrap();
            }
            Ok(None)
        }

        Statement::Break { .. } => {
            let exit_block = c.loop_exit_stack.last().ok_or("break outside of loop")?;
            c.builder.build_unconditional_branch(*exit_block).unwrap();
            Ok(None)
        }

        Statement::CompoundAssign {
            target, op, value, ..
        } => {
            if target.segments.len() != 1 {
                return Err("compound assignment to fields not yet supported".to_string());
            }
            let name = &target.segments[0];

            let (ptr, var_ty, _) = c
                .variables
                .get(name)
                .ok_or_else(|| format!("undefined variable: {name}"))?
                .clone();

            let llvm_ty = to_llvm_type(&var_ty, c.context, &c.struct_types)
                .ok_or("cannot load variable of unsupported type")?;
            let current = c.builder.build_load(llvm_ty, ptr, name).unwrap();
            let rhs = compile_expr(c, value, function)?
                .ok_or("compound assignment value produced no value")?;

            if current.is_int_value() && rhs.is_int_value() {
                let l = current.into_int_value();
                let r = rhs.into_int_value();
                let result = match op {
                    expo_ast::ast::CompoundOp::Add => {
                        c.builder.build_int_add(l, r, "cadd").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Sub => {
                        c.builder.build_int_sub(l, r, "csub").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Mul => {
                        c.builder.build_int_mul(l, r, "cmul").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Div => {
                        c.builder.build_int_signed_div(l, r, "cdiv").unwrap()
                    }
                };
                c.builder.build_store(ptr, result).unwrap();
            } else if current.is_float_value() && rhs.is_float_value() {
                let l = current.into_float_value();
                let r = rhs.into_float_value();
                let result = match op {
                    expo_ast::ast::CompoundOp::Add => {
                        c.builder.build_float_add(l, r, "cfadd").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Sub => {
                        c.builder.build_float_sub(l, r, "cfsub").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Mul => {
                        c.builder.build_float_mul(l, r, "cfmul").unwrap()
                    }
                    expo_ast::ast::CompoundOp::Div => {
                        c.builder.build_float_div(l, r, "cfdiv").unwrap()
                    }
                };
                c.builder.build_store(ptr, result).unwrap();
            } else {
                return Err("compound assignment requires matching numeric types".to_string());
            }

            Ok(None)
        }
    }
}

fn compile_field_assignment<'ctx>(
    c: &mut Compiler<'ctx>,
    segments: &[String],
    val: BasicValueEnum<'ctx>,
) -> Result<(), String> {
    let var_name = &segments[0];
    let (mut ptr, ty, _) = c
        .variables
        .get(var_name)
        .ok_or_else(|| format!("undefined variable: {var_name}"))?
        .clone();

    let mut current_type = ty;

    for field_name in &segments[1..] {
        let struct_name = match &current_type {
            Type::Struct(n) => n.clone(),
            Type::GenericInstance {
                base,
                type_args,
                kind: GenericKind::Struct,
            } => mangle_name(base, type_args),
            _ => {
                return Err(format!(
                    "cannot access field `{field_name}` on non-struct type"
                ));
            }
        };

        let struct_type = *c
            .struct_types
            .get(&struct_name)
            .ok_or_else(|| format!("unknown struct type: {struct_name}"))?;

        let field_idx = c
            .get_field_index(&struct_name, field_name)
            .ok_or_else(|| format!("unknown field `{field_name}` on struct `{struct_name}`"))?;

        let field_ty = c
            .get_field_type(&struct_name, field_name)
            .ok_or_else(|| format!("unknown field `{field_name}` on struct `{struct_name}`"))?;

        ptr = c
            .builder
            .build_struct_gep(
                struct_type,
                ptr,
                field_idx,
                &format!("{var_name}.{field_name}"),
            )
            .unwrap();

        current_type = field_ty;
    }

    c.builder.build_store(ptr, val).unwrap();
    Ok(())
}

/// Attempts to derive the Expo type directly from the expression AST. Returns
/// `Some(Type::Function{..})` for closures so the variable is stored with the
/// correct callable type rather than being misidentified as a string pointer.
fn infer_type_from_expr(c: &Compiler, expr: &Expr) -> Option<Type> {
    if let Expr::MethodCall {
        receiver,
        method,
        args,
        ..
    } = expr
        && let Expr::Ident {
            name: type_name, ..
        } = receiver.as_ref()
    {
        let is_type_name =
            c.type_ctx.structs.contains_key(type_name) || c.type_ctx.enums.contains_key(type_name);
        if is_type_name {
            return infer_static_method_return_type(c, type_name, method, args);
        }
    }
    if let Expr::Closure {
        params,
        return_type,
        ..
    } = expr
    {
        let param_types: Vec<Type> = params
            .iter()
            .map(|p| match p {
                ClosureParam::Name {
                    type_expr: Some(te),
                    ..
                } => c.resolve_type_expr(te),
                _ => Type::Primitive(expo_typecheck::types::Primitive::I32),
            })
            .collect();
        let ret = match return_type {
            Some(te) => c.resolve_type_expr(te),
            None => Type::Unit,
        };
        return Some(Type::Function {
            params: param_types,
            return_type: Box::new(ret),
        });
    }
    if let Expr::Ident { name, .. } = expr
        && let Some(sig) = c.type_ctx.functions.get(name)
        && sig.type_params.is_empty()
    {
        return Some(Type::Function {
            params: sig.params.iter().map(|p| p.ty.clone()).collect(),
            return_type: Box::new(sig.return_type.clone()),
        });
    }
    if let Expr::Call { callee, .. } = expr
        && let Expr::Ident { name, .. } = callee.as_ref()
        && let Some(sig) = c.type_ctx.functions.get(name)
        && sig.type_params.is_empty()
    {
        return Some(sig.return_type.clone());
    }
    if matches!(expr, Expr::Receive { .. }) {
        return c.process_msg_type.clone();
    }
    None
}

/// Parses a mangled enum name like `Option_$i32$` and reconstructs a
/// `GenericInstance` type so unification works in generic function calls.
fn parse_mangled_type_arg(s: &str, c: &Compiler) -> Type {
    use expo_typecheck::types::Primitive;

    if s == "unit" {
        return Type::Unit;
    }
    if let Some(p) = Primitive::from_name(s) {
        return Type::Primitive(p);
    }
    if let Some(gi) = try_parse_mangled_generic(s, c) {
        return gi;
    }
    if let Some(body) = s.strip_prefix("fn_")
        && let Some(dunder_pos) = body.rfind("__")
    {
        let params_str = &body[..dunder_pos];
        let return_str = &body[dunder_pos + 2..];
        let params = if params_str.is_empty() {
            Vec::new()
        } else {
            params_str
                .split('_')
                .map(|p| parse_mangled_type_arg(p, c))
                .collect()
        };
        let return_type = Box::new(parse_mangled_type_arg(return_str, c));
        return Type::Function {
            params,
            return_type,
        };
    }
    if c.type_ctx.structs.contains_key(s) || c.mono_struct_info.contains_key(s) {
        return Type::Struct(s.to_string());
    }
    if c.type_ctx.enums.contains_key(s) || c.mono_enum_variants.contains_key(s) {
        return Type::Enum(s.to_string());
    }
    Type::Unknown
}

fn try_parse_mangled_generic(mangled: &str, c: &Compiler) -> Option<Type> {
    use expo_typecheck::types::GenericKind;

    let sep = mangled.find("_$")?;
    let base = &mangled[..sep];
    if !mangled.ends_with('$') {
        return None;
    }
    let kind = if c.type_ctx.generic_enum_asts.contains_key(base) {
        GenericKind::Enum
    } else if c.type_ctx.generic_struct_asts.contains_key(base) {
        GenericKind::Struct
    } else {
        return None;
    };
    let inner = &mangled[sep + 2..mangled.len() - 1];
    let type_args: Vec<Type> = inner
        .split('.')
        .map(|s| parse_mangled_type_arg(s, c))
        .collect();
    Some(Type::GenericInstance {
        base: base.to_string(),
        type_args,
        kind,
    })
}

fn parse_mangled_enum_type(mangled: &str, c: &Compiler) -> Option<Type> {
    let gi = try_parse_mangled_generic(mangled, c)?;
    match &gi {
        Type::GenericInstance {
            kind: expo_typecheck::types::GenericKind::Enum,
            ..
        } => Some(gi),
        _ => None,
    }
}

fn expo_type_from_mangled_llvm_struct_name(c: &Compiler, name_str: &str) -> Option<Type> {
    if c.type_ctx.structs.contains_key(name_str) {
        return Some(Type::Struct(name_str.to_string()));
    }
    if c.mono_struct_info.contains_key(name_str) {
        if let Some(gi) = try_parse_mangled_generic(name_str, c) {
            return Some(gi);
        }
        return Some(Type::Struct(name_str.to_string()));
    }
    if c.type_ctx.enums.contains_key(name_str) {
        return Some(Type::Enum(name_str.to_string()));
    }
    if c.mono_enum_variants.contains_key(name_str) {
        if let Some(gi) = parse_mangled_enum_type(name_str, c) {
            return Some(gi);
        }
        return Some(Type::Enum(name_str.to_string()));
    }
    for ty in c.type_ctx.type_aliases.values() {
        if let Type::Union(_) = ty
            && mangle_type(ty) == name_str
        {
            return Some(ty.clone());
        }
    }
    None
}

/// Reconstructs an Expo type from an LLVM value by inspecting bit widths and
/// struct names. Used when assigning to a new variable without a type annotation.
pub fn infer_type_from_llvm<'ctx>(c: &Compiler<'ctx>, val: &BasicValueEnum<'ctx>) -> Type {
    use expo_typecheck::types::Primitive;

    if val.is_int_value() {
        match val.into_int_value().get_type().get_bit_width() {
            1 => Type::Primitive(Primitive::Bool),
            8 => Type::Primitive(Primitive::I8),
            16 => Type::Primitive(Primitive::I16),
            32 => Type::Primitive(Primitive::I32),
            64 => Type::Primitive(Primitive::I64),
            _ => Type::Unknown,
        }
    } else if val.is_float_value() {
        Type::Primitive(Primitive::F64)
    } else if val.is_struct_value() {
        let sv = val.into_struct_value();
        let st = sv.get_type();
        if let Some(name) = st.get_name()
            && let Ok(name_str) = name.to_str()
            && let Some(ty) = expo_type_from_mangled_llvm_struct_name(c, name_str)
        {
            return ty;
        }
        Type::Unknown
    } else if val.is_pointer_value() {
        // LLVM opaque pointers do not expose a pointee struct type; heap strings and
        // other pointers both look like `ptr`. Call sites that need a precise type
        // should get it from `infer_type_from_expr` instead of LLVM inspection.
        Type::Primitive(Primitive::String)
    } else {
        Type::Unknown
    }
}

pub(crate) fn coerce_numeric<'ctx>(
    c: &Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    target: &Type,
) -> BasicValueEnum<'ctx> {
    let Type::Primitive(target_prim) = target else {
        return val;
    };

    if val.is_int_value() && target_prim.is_integer() {
        let iv = val.into_int_value();
        let src_bits = iv.get_type().get_bit_width();
        let dst_bits = int_bit_width(target_prim);
        if src_bits == dst_bits {
            return iv.into();
        }
        let dst_type = c.context.custom_width_int_type(dst_bits);
        if dst_bits < src_bits {
            return c
                .builder
                .build_int_truncate(iv, dst_type, "trunc")
                .unwrap()
                .into();
        }
        let signed = matches!(
            target_prim,
            Primitive::I8 | Primitive::I16 | Primitive::I32 | Primitive::I64
        );
        if signed {
            c.builder
                .build_int_s_extend(iv, dst_type, "sext")
                .unwrap()
                .into()
        } else {
            c.builder
                .build_int_z_extend(iv, dst_type, "zext")
                .unwrap()
                .into()
        }
    } else if val.is_float_value() && target_prim.is_float() {
        let fv = val.into_float_value();
        let dst_is_f64 = *target_prim == Primitive::F64;
        if (fv.get_type() == c.context.f64_type()) == dst_is_f64 {
            return fv.into();
        }
        if dst_is_f64 {
            c.builder
                .build_float_ext(fv, c.context.f64_type(), "fpext")
                .unwrap()
                .into()
        } else {
            c.builder
                .build_float_trunc(fv, c.context.f32_type(), "fptrunc")
                .unwrap()
                .into()
        }
    } else {
        val
    }
}

fn int_bit_width(p: &Primitive) -> u32 {
    match p {
        Primitive::I8 | Primitive::U8 => 8,
        Primitive::I16 | Primitive::U16 => 16,
        Primitive::I32 | Primitive::U32 => 32,
        Primitive::I64 | Primitive::U64 => 64,
        _ => 0,
    }
}

/// When a list literal `[a, b, c]` is assigned to a non-List type that
/// implements `ListLiteral<T>` (e.g. `Set<T>`), calls `from_list` to convert.
fn convert_list_literal_if_needed<'ctx>(
    c: &mut Compiler<'ctx>,
    list_val: BasicValueEnum<'ctx>,
    target_type: &Type,
) -> Result<BasicValueEnum<'ctx>, String> {
    let (base, type_args) = match target_type {
        Type::GenericInstance {
            base, type_args, ..
        } if base != "List" => (base.clone(), type_args.clone()),
        _ => return Ok(list_val),
    };

    let target_mangled = mangle_name(&base, &type_args);
    let from_list_fn_name = format!("{target_mangled}_from_list");
    if !c.functions.contains_key(&from_list_fn_name) {
        c.monomorphize_impl_method(&base, "from_list", &type_args)?;
    }
    let from_list_fn = *c
        .functions
        .get(&from_list_fn_name)
        .ok_or_else(|| format!("{base} does not implement ListLiteral (no from_list)"))?;

    let result = c
        .builder
        .build_call(from_list_fn, &[list_val.into()], "from_list")
        .unwrap()
        .try_as_basic_value()
        .left()
        .ok_or("from_list returned void")?;

    Ok(result)
}

/// Wraps a concrete value into a tagged union representation.
/// Allocates the union struct `{ i8 tag, [N x i8] payload }`, writes the tag
/// (index of `source` in the union members), stores the value into the payload
/// area, and loads the whole struct back as a value.
pub(crate) fn compile_union_wrap<'ctx>(
    c: &mut Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    source: &Type,
    target_union: &Type,
) -> Result<BasicValueEnum<'ctx>, String> {
    let Type::Union(members) = target_union else {
        return Ok(val);
    };

    let source_mangled = mangle_type(source);
    let union_mangled = mangle_type(target_union);

    let tag = members
        .iter()
        .position(|m| mangle_type(m) == source_mangled)
        .ok_or_else(|| {
            format!(
                "{} is not a member of union {}",
                source.display(),
                target_union.display()
            )
        })? as u64;

    let union_llvm_ty = *c
        .struct_types
        .get(&union_mangled)
        .ok_or_else(|| format!("union type {} not registered", union_mangled))?;

    let alloca = c.builder.build_alloca(union_llvm_ty, "union_wrap").unwrap();

    let tag_ptr = c
        .builder
        .build_struct_gep(union_llvm_ty, alloca, 0, "tag_ptr")
        .unwrap();
    let tag_val = c.context.i8_type().const_int(tag, false);
    c.builder.build_store(tag_ptr, tag_val).unwrap();

    if union_llvm_ty.count_fields() > 1 {
        let payload_ptr = c
            .builder
            .build_struct_gep(union_llvm_ty, alloca, 1, "payload_ptr")
            .unwrap();
        c.builder.build_store(payload_ptr, val).unwrap();
    }

    let result = c
        .builder
        .build_load(union_llvm_ty, alloca, "union_val")
        .unwrap();
    Ok(result)
}

/// Applies a recorded coercion to a compiled value, if one exists for the
/// given expression span. Currently handles union widening.
pub(crate) fn apply_coercion<'ctx>(
    c: &mut Compiler<'ctx>,
    val: BasicValueEnum<'ctx>,
    expr: &Expr,
) -> Result<BasicValueEnum<'ctx>, String> {
    let span = expr_span(expr);
    if let Some(coercion) = c.type_ctx.coercions.get(&span).cloned() {
        match coercion {
            Coercion::UnionWiden { source, target } => compile_union_wrap(c, val, &source, &target),
        }
    } else {
        Ok(val)
    }
}

fn expr_span(expr: &Expr) -> expo_ast::span::Span {
    match expr {
        Expr::Arena { span, .. }
        | Expr::Binary { span, .. }
        | Expr::BinaryLiteral { span, .. }
        | Expr::Call { span, .. }
        | Expr::Closure { span, .. }
        | Expr::Cond { span, .. }
        | Expr::EnumConstruction { span, .. }
        | Expr::FieldAccess { span, .. }
        | Expr::For { span, .. }
        | Expr::Group { span, .. }
        | Expr::Ident { span, .. }
        | Expr::If { span, .. }
        | Expr::List { span, .. }
        | Expr::Map { span, .. }
        | Expr::Literal { span, .. }
        | Expr::Loop { span, .. }
        | Expr::Match { span, .. }
        | Expr::MethodCall { span, .. }
        | Expr::Receive { span, .. }
        | Expr::Self_ { span, .. }
        | Expr::ShortClosure { span, .. }
        | Expr::Spawn { span, .. }
        | Expr::String { span, .. }
        | Expr::StructConstruction { span, .. }
        | Expr::Ternary { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Unless { span, .. }
        | Expr::While { span, .. } => *span,
    }
}

fn ownership_for_expr(expr: &Expr, ty: &Type) -> Ownership {
    if !matches!(ty, Type::Primitive(Primitive::String)) {
        return Ownership::Unowned;
    }
    match expr {
        Expr::String { parts, .. } => {
            let has_interpolation = parts
                .iter()
                .any(|p| matches!(p, expo_ast::ast::StringPart::Interpolation { .. }));
            if has_interpolation {
                Ownership::Owned
            } else {
                Ownership::Unowned
            }
        }
        Expr::Receive { .. } => Ownership::Owned,
        _ => Ownership::Unowned,
    }
}
