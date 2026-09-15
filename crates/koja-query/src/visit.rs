//! Read-only traversal of the AST.
//!
//! [`Visitor`] has one method per node family. Each default calls the
//! matching `walk_*` function, which visits the node's children. An
//! implementation overrides the methods it cares about and calls the
//! `walk_*` function itself when it also wants the children.
//!
//! Parents are visited before children, so a visitor that records the
//! last node containing a position ends with the innermost one.

use koja_ast::ast::*;

/// Callbacks for each AST node family. Every method has a default
/// that walks into the children.
pub trait Visitor<'ast> {
    fn visit_file(&mut self, file: &'ast File) {
        walk_file(self, file);
    }

    fn visit_item(&mut self, item: &'ast Item) {
        walk_item(self, item);
    }

    fn visit_function(&mut self, function: &'ast Function) {
        walk_function(self, function);
    }

    fn visit_protocol_method(&mut self, method: &'ast ProtocolMethod) {
        walk_protocol_method(self, method);
    }

    fn visit_type_param(&mut self, type_param: &'ast TypeParam) {
        walk_type_param(self, type_param);
    }

    fn visit_param(&mut self, param: &'ast Param) {
        walk_param(self, param);
    }

    fn visit_closure_param(&mut self, param: &'ast ClosureParam) {
        walk_closure_param(self, param);
    }

    fn visit_statement(&mut self, statement: &'ast Statement) {
        walk_statement(self, statement);
    }

    fn visit_lvalue(&mut self, _lvalue: &'ast LValue) {}

    fn visit_expr(&mut self, expr: &'ast Expr) {
        walk_expr(self, expr);
    }

    fn visit_pattern(&mut self, pattern: &'ast Pattern) {
        walk_pattern(self, pattern);
    }

    fn visit_type_expr(&mut self, type_expr: &'ast TypeExpr) {
        walk_type_expr(self, type_expr);
    }
}

pub fn walk_file<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, file: &'ast File) {
    for item in &file.items {
        v.visit_item(item);
    }
    if let Some(body) = &file.body {
        walk_body(v, body);
    }
}

pub fn walk_item<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, item: &'ast Item) {
    match item {
        Item::Alias(_) => {}
        Item::Builtin(decl) => {
            walk_type_params(v, &decl.type_params);
            for function in &decl.functions {
                v.visit_function(function);
            }
            for test in &decl.tests {
                walk_body(v, &test.body);
            }
        }
        Item::Constant(constant) => {
            if let Some(type_expr) = &constant.type_annotation {
                v.visit_type_expr(type_expr);
            }
            v.visit_expr(&constant.value);
        }
        Item::Enum(decl) => {
            walk_type_params(v, &decl.type_params);
            for conformance in &decl.conformances {
                v.visit_type_expr(conformance);
            }
            for variant in &decl.variants {
                match &variant.data {
                    EnumVariantData::Unit => {}
                    EnumVariantData::Tuple(types) => {
                        for type_expr in types {
                            v.visit_type_expr(type_expr);
                        }
                    }
                    EnumVariantData::Struct(fields) => walk_struct_fields(v, fields),
                }
            }
            for function in &decl.functions {
                v.visit_function(function);
            }
            for nested in &decl.nested {
                v.visit_item(nested);
            }
            for test in &decl.tests {
                walk_body(v, &test.body);
            }
        }
        Item::Extend(block) => {
            v.visit_type_expr(&block.target);
            walk_impl_members(v, &block.members);
            for test in &block.tests {
                walk_body(v, &test.body);
            }
        }
        Item::Function(function) => v.visit_function(function),
        Item::Impl(block) => {
            v.visit_type_expr(&block.target);
            walk_type_params(v, &block.target_bounds);
            v.visit_type_expr(&block.trait_expr);
            walk_impl_members(v, &block.members);
            for test in &block.tests {
                walk_body(v, &test.body);
            }
        }
        Item::Protocol(decl) => {
            walk_type_params(v, &decl.type_params);
            for method in &decl.methods {
                v.visit_protocol_method(method);
            }
        }
        Item::Struct(decl) => {
            walk_type_params(v, &decl.type_params);
            for conformance in &decl.conformances {
                v.visit_type_expr(conformance);
            }
            walk_struct_fields(v, &decl.fields);
            for function in &decl.functions {
                v.visit_function(function);
            }
            for nested in &decl.nested {
                v.visit_item(nested);
            }
            for test in &decl.tests {
                walk_body(v, &test.body);
            }
        }
        Item::Test(test) => walk_body(v, &test.body),
        Item::TypeAlias(alias) => v.visit_type_expr(&alias.type_expr),
    }
}

fn walk_impl_members<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, members: &'ast [ImplMember]) {
    for member in members {
        match member {
            ImplMember::Function(function) => v.visit_function(function),
            ImplMember::TypeAlias(alias) => v.visit_type_expr(&alias.type_expr),
        }
    }
}

fn walk_struct_fields<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, fields: &'ast [StructField]) {
    for field in fields {
        v.visit_type_expr(&field.type_expr);
        if let Some(default) = &field.default {
            v.visit_expr(default);
        }
    }
}

fn walk_type_params<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, type_params: &'ast [TypeParam]) {
    for type_param in type_params {
        v.visit_type_param(type_param);
    }
}

pub fn walk_function<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, function: &'ast Function) {
    walk_type_params(v, &function.type_params);
    for param in &function.params {
        v.visit_param(param);
    }
    if let Some(return_type) = &function.return_type {
        v.visit_type_expr(return_type);
    }
    if let Some(error_type) = &function.error_type {
        v.visit_type_expr(error_type);
    }
    if let Some(body) = &function.body {
        walk_body(v, body);
    }
}

pub fn walk_protocol_method<'ast, V: Visitor<'ast> + ?Sized>(
    v: &mut V,
    method: &'ast ProtocolMethod,
) {
    walk_type_params(v, &method.type_params);
    for param in &method.params {
        v.visit_param(param);
    }
    if let Some(return_type) = &method.return_type {
        v.visit_type_expr(return_type);
    }
    if let Some(error_type) = &method.error_type {
        v.visit_type_expr(error_type);
    }
    if let Some(body) = &method.body {
        walk_body(v, body);
    }
}

pub fn walk_type_param<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, type_param: &'ast TypeParam) {
    for bound in &type_param.bounds {
        v.visit_type_expr(bound);
    }
}

pub fn walk_param<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, param: &'ast Param) {
    if let Param::Regular {
        type_expr, default, ..
    } = param
    {
        v.visit_type_expr(type_expr);
        if let Some(default) = default {
            v.visit_expr(default);
        }
    }
}

pub fn walk_closure_param<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, param: &'ast ClosureParam) {
    if let ClosureParam::Name {
        type_expr: Some(type_expr),
        ..
    } = param
    {
        v.visit_type_expr(type_expr);
    }
}

pub fn walk_body<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, body: &'ast [Statement]) {
    for statement in body {
        v.visit_statement(statement);
    }
}

pub fn walk_statement<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, statement: &'ast Statement) {
    match statement {
        Statement::Expr(expr) => v.visit_expr(expr),
        Statement::Assignment {
            target,
            type_annotation,
            value,
            ..
        } => {
            v.visit_lvalue(target);
            if let Some(type_expr) = type_annotation {
                v.visit_type_expr(type_expr);
            }
            v.visit_expr(value);
        }
        Statement::CompoundAssign { target, value, .. } => {
            v.visit_lvalue(target);
            v.visit_expr(value);
        }
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                v.visit_expr(value);
            }
        }
        Statement::Break { .. } => {}
        Statement::Destructure { pattern, value, .. } => {
            v.visit_pattern(pattern);
            v.visit_expr(value);
        }
    }
}

fn walk_args<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, args: &'ast [Arg]) {
    for arg in args {
        v.visit_expr(&arg.value);
    }
}

fn walk_match_arms<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, arms: &'ast [MatchArm]) {
    for arm in arms {
        v.visit_pattern(&arm.pattern);
        if let Some(guard) = &arm.guard {
            v.visit_expr(guard);
        }
        walk_body(v, &arm.body);
    }
}

fn walk_binary_segments<'ast, V: Visitor<'ast> + ?Sized>(
    v: &mut V,
    segments: &'ast [BinarySegment],
) {
    for segment in segments {
        v.visit_expr(&segment.value);
        if let Some(size) = &segment.size {
            v.visit_expr(size);
        }
        if let Some(type_ann) = &segment.type_ann {
            v.visit_type_expr(type_ann);
        }
    }
}

fn walk_field_inits<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, fields: &'ast [FieldInit]) {
    for field in fields {
        v.visit_expr(&field.value);
    }
}

pub fn walk_expr<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, expr: &'ast Expr) {
    match &expr.kind {
        ExprKind::Binary { left, right, .. } => {
            v.visit_expr(left);
            v.visit_expr(right);
        }
        ExprKind::BinaryLiteral { segments } => walk_binary_segments(v, segments),
        ExprKind::Call { callee, args, .. } => {
            v.visit_expr(callee);
            walk_args(v, args);
        }
        ExprKind::Closure {
            params,
            return_type,
            body,
        } => {
            for param in params {
                v.visit_closure_param(param);
            }
            if let Some(return_type) = return_type {
                v.visit_type_expr(return_type);
            }
            walk_body(v, body);
        }
        ExprKind::Cond { arms, else_body } => {
            for arm in arms {
                v.visit_expr(&arm.condition);
                walk_body(v, &arm.body);
            }
            if let Some(else_body) = else_body {
                walk_body(v, else_body);
            }
        }
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Unit => {}
            EnumConstructionData::Tuple(values) => {
                for value in values {
                    v.visit_expr(value);
                }
            }
            EnumConstructionData::Struct(fields) => walk_field_inits(v, fields),
        },
        ExprKind::Fail { value } => v.visit_expr(value),
        ExprKind::FieldAccess { receiver, .. } => v.visit_expr(receiver),
        ExprKind::For {
            pattern,
            iterable,
            body,
        } => {
            v.visit_pattern(pattern);
            v.visit_expr(iterable);
            walk_body(v, body);
        }
        ExprKind::Group { expr } => v.visit_expr(expr),
        ExprKind::Assert {
            condition, message, ..
        } => {
            v.visit_expr(condition);
            if let Some(message) = message {
                v.visit_expr(message);
            }
        }
        ExprKind::Ident { .. } => {}
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            v.visit_expr(condition);
            walk_body(v, then_body);
            if let Some(else_body) = else_body {
                walk_body(v, else_body);
            }
        }
        ExprKind::List { elements } | ExprKind::Tuple { elements } => {
            for element in elements {
                v.visit_expr(element);
            }
        }
        ExprKind::Map { entries } => {
            for (key, value) in entries {
                v.visit_expr(key);
                v.visit_expr(value);
            }
        }
        ExprKind::Literal { .. } => {}
        ExprKind::Loop { body } => walk_body(v, body),
        ExprKind::Match { subject, arms } => {
            v.visit_expr(subject);
            walk_match_arms(v, arms);
        }
        ExprKind::NamedFunctionReference { .. } => {}
        ExprKind::MethodCall { receiver, args, .. } => {
            v.visit_expr(receiver);
            walk_args(v, args);
        }
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            walk_match_arms(v, arms);
            if let Some(timeout) = after_timeout {
                v.visit_expr(timeout);
            }
            walk_body(v, after_body);
        }
        ExprKind::Rescue {
            subject, handler, ..
        } => {
            v.visit_expr(subject);
            v.visit_expr(handler);
        }
        ExprKind::Self_ { .. } => {}
        ExprKind::ShortClosure { params, body } => {
            for param in params {
                v.visit_closure_param(param);
            }
            v.visit_expr(body);
        }
        ExprKind::Spawn { expr } | ExprKind::Try { expr } => v.visit_expr(expr),
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    v.visit_expr(expr);
                }
            }
        }
        ExprKind::StructConstruction { fields, .. } => walk_field_inits(v, fields),
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            v.visit_expr(condition);
            v.visit_expr(then_expr);
            v.visit_expr(else_expr);
        }
        ExprKind::Unary { operand, .. } => v.visit_expr(operand),
        ExprKind::While { condition, body } => {
            v.visit_expr(condition);
            walk_body(v, body);
        }
    }
}

pub fn walk_pattern<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, pattern: &'ast Pattern) {
    match pattern {
        Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::Binding { .. } => {}
        Pattern::Binary { segments, .. } => walk_binary_segments(v, segments),
        Pattern::EnumUnit { .. } => {}
        Pattern::EnumTuple { elements, .. }
        | Pattern::Constructor { elements, .. }
        | Pattern::List { elements, .. }
        | Pattern::Tuple { elements, .. } => {
            for element in elements {
                v.visit_pattern(element);
            }
        }
        Pattern::EnumStruct { fields, .. } | Pattern::Struct { fields, .. } => {
            for field in fields {
                v.visit_pattern(&field.pattern);
            }
        }
        Pattern::TypedBinding { type_expr, .. } => v.visit_type_expr(type_expr),
        Pattern::Or { patterns, .. } => {
            for pattern in patterns {
                v.visit_pattern(pattern);
            }
        }
    }
}

pub fn walk_type_expr<'ast, V: Visitor<'ast> + ?Sized>(v: &mut V, type_expr: &'ast TypeExpr) {
    match type_expr {
        TypeExpr::Named { .. } | TypeExpr::Unit { .. } | TypeExpr::Self_ { .. } => {}
        TypeExpr::Generic { args, .. } => {
            for arg in args {
                v.visit_type_expr(arg);
            }
        }
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            for param in params {
                v.visit_type_expr(param);
            }
            v.visit_type_expr(return_type);
        }
        TypeExpr::Tuple { elements, .. }
        | TypeExpr::Union {
            types: elements, ..
        } => {
            for element in elements {
                v.visit_type_expr(element);
            }
        }
    }
}
