//! Mutable traversal of the AST.
//!
//! [`VisitorMut`] mirrors [`crate::visit::Visitor`] over `&mut` nodes.
//! Each default calls the matching `walk_*_mut` function, which visits
//! the node's children in the same order as the read-only walk. An
//! implementation overrides the methods it cares about and calls the
//! `walk_*_mut` function itself when it also wants the children.
//!
//! The two modules must stay in step. A new node variant needs an arm
//! in both `walk_*` functions.

use crate::ast::*;

/// Callbacks for each AST node family over mutable nodes. Every method
/// has a default that walks into the children.
pub trait VisitorMut {
    fn visit_file_mut(&mut self, file: &mut File) {
        walk_file_mut(self, file);
    }

    fn visit_item_mut(&mut self, item: &mut Item) {
        walk_item_mut(self, item);
    }

    fn visit_function_mut(&mut self, function: &mut Function) {
        walk_function_mut(self, function);
    }

    fn visit_protocol_method_mut(&mut self, method: &mut ProtocolMethod) {
        walk_protocol_method_mut(self, method);
    }

    fn visit_test_mut(&mut self, test: &mut TestDecl) {
        walk_test_mut(self, test);
    }

    fn visit_type_param_mut(&mut self, type_param: &mut TypeParam) {
        walk_type_param_mut(self, type_param);
    }

    fn visit_param_mut(&mut self, param: &mut Param) {
        walk_param_mut(self, param);
    }

    fn visit_closure_param_mut(&mut self, param: &mut ClosureParam) {
        walk_closure_param_mut(self, param);
    }

    fn visit_statement_mut(&mut self, statement: &mut Statement) {
        walk_statement_mut(self, statement);
    }

    fn visit_lvalue_mut(&mut self, _lvalue: &mut LValue) {}

    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        walk_expr_mut(self, expr);
    }

    fn visit_match_arm_mut(&mut self, arm: &mut MatchArm) {
        walk_match_arm_mut(self, arm);
    }

    fn visit_pattern_mut(&mut self, pattern: &mut Pattern) {
        walk_pattern_mut(self, pattern);
    }

    fn visit_type_expr_mut(&mut self, type_expr: &mut TypeExpr) {
        walk_type_expr_mut(self, type_expr);
    }
}

pub fn walk_file_mut<V: VisitorMut + ?Sized>(v: &mut V, file: &mut File) {
    for item in &mut file.items {
        v.visit_item_mut(item);
    }
    if let Some(body) = &mut file.body {
        walk_body_mut(v, body);
    }
}

pub fn walk_item_mut<V: VisitorMut + ?Sized>(v: &mut V, item: &mut Item) {
    match item {
        Item::Alias(_) => {}
        Item::Builtin(decl) => {
            walk_type_params_mut(v, &mut decl.type_params);
            for function in &mut decl.functions {
                v.visit_function_mut(function);
            }
            for nested in &mut decl.nested {
                v.visit_item_mut(nested);
            }
            walk_tests_mut(v, &mut decl.tests);
        }
        Item::Constant(constant) => {
            if let Some(type_expr) = &mut constant.type_annotation {
                v.visit_type_expr_mut(type_expr);
            }
            v.visit_expr_mut(&mut constant.value);
        }
        Item::Enum(decl) => {
            walk_type_params_mut(v, &mut decl.type_params);
            for conformance in &mut decl.conformances {
                v.visit_type_expr_mut(conformance);
            }
            for variant in &mut decl.variants {
                match &mut variant.data {
                    EnumVariantData::Unit => {}
                    EnumVariantData::Tuple(types) => {
                        for type_expr in types {
                            v.visit_type_expr_mut(type_expr);
                        }
                    }
                    EnumVariantData::Struct(fields) => walk_struct_fields_mut(v, fields),
                }
            }
            for function in &mut decl.functions {
                v.visit_function_mut(function);
            }
            for nested in &mut decl.nested {
                v.visit_item_mut(nested);
            }
            walk_tests_mut(v, &mut decl.tests);
        }
        Item::Extend(block) => {
            v.visit_type_expr_mut(&mut block.target);
            walk_impl_members_mut(v, &mut block.members);
            walk_tests_mut(v, &mut block.tests);
        }
        Item::Function(function) => v.visit_function_mut(function),
        Item::Impl(block) => {
            v.visit_type_expr_mut(&mut block.target);
            walk_type_params_mut(v, &mut block.target_bounds);
            v.visit_type_expr_mut(&mut block.trait_expr);
            walk_impl_members_mut(v, &mut block.members);
            walk_tests_mut(v, &mut block.tests);
        }
        Item::Protocol(decl) => {
            walk_type_params_mut(v, &mut decl.type_params);
            for method in &mut decl.methods {
                v.visit_protocol_method_mut(method);
            }
        }
        Item::Struct(decl) => {
            walk_type_params_mut(v, &mut decl.type_params);
            for conformance in &mut decl.conformances {
                v.visit_type_expr_mut(conformance);
            }
            walk_struct_fields_mut(v, &mut decl.fields);
            for function in &mut decl.functions {
                v.visit_function_mut(function);
            }
            for nested in &mut decl.nested {
                v.visit_item_mut(nested);
            }
            walk_tests_mut(v, &mut decl.tests);
        }
        Item::Test(test) => v.visit_test_mut(test),
        Item::TypeAlias(alias) => v.visit_type_expr_mut(&mut alias.type_expr),
    }
}

fn walk_tests_mut<V: VisitorMut + ?Sized>(v: &mut V, tests: &mut [TestDecl]) {
    for test in tests {
        v.visit_test_mut(test);
    }
}

fn walk_impl_members_mut<V: VisitorMut + ?Sized>(v: &mut V, members: &mut [ImplMember]) {
    for member in members {
        match member {
            ImplMember::Function(function) => v.visit_function_mut(function),
            ImplMember::TypeAlias(alias) => v.visit_type_expr_mut(&mut alias.type_expr),
        }
    }
}

fn walk_struct_fields_mut<V: VisitorMut + ?Sized>(v: &mut V, fields: &mut [StructField]) {
    for field in fields {
        v.visit_type_expr_mut(&mut field.type_expr);
        if let Some(default) = &mut field.default {
            v.visit_expr_mut(default);
        }
    }
}

fn walk_type_params_mut<V: VisitorMut + ?Sized>(v: &mut V, type_params: &mut [TypeParam]) {
    for type_param in type_params {
        v.visit_type_param_mut(type_param);
    }
}

pub fn walk_function_mut<V: VisitorMut + ?Sized>(v: &mut V, function: &mut Function) {
    walk_type_params_mut(v, &mut function.type_params);
    for param in &mut function.params {
        v.visit_param_mut(param);
    }
    if let Some(return_type) = &mut function.return_type {
        v.visit_type_expr_mut(return_type);
    }
    if let Some(error_type) = &mut function.error_type {
        v.visit_type_expr_mut(error_type);
    }
    if let Some(body) = &mut function.body {
        walk_body_mut(v, body);
    }
}

pub fn walk_protocol_method_mut<V: VisitorMut + ?Sized>(v: &mut V, method: &mut ProtocolMethod) {
    walk_type_params_mut(v, &mut method.type_params);
    for param in &mut method.params {
        v.visit_param_mut(param);
    }
    if let Some(return_type) = &mut method.return_type {
        v.visit_type_expr_mut(return_type);
    }
    if let Some(error_type) = &mut method.error_type {
        v.visit_type_expr_mut(error_type);
    }
    if let Some(body) = &mut method.body {
        walk_body_mut(v, body);
    }
}

pub fn walk_test_mut<V: VisitorMut + ?Sized>(v: &mut V, test: &mut TestDecl) {
    walk_body_mut(v, &mut test.body);
}

pub fn walk_type_param_mut<V: VisitorMut + ?Sized>(v: &mut V, type_param: &mut TypeParam) {
    for bound in &mut type_param.bounds {
        v.visit_type_expr_mut(bound);
    }
}

pub fn walk_param_mut<V: VisitorMut + ?Sized>(v: &mut V, param: &mut Param) {
    if let Param::Regular {
        type_expr, default, ..
    } = param
    {
        v.visit_type_expr_mut(type_expr);
        if let Some(default) = default {
            v.visit_expr_mut(default);
        }
    }
}

pub fn walk_closure_param_mut<V: VisitorMut + ?Sized>(v: &mut V, param: &mut ClosureParam) {
    if let ClosureParam::Name {
        type_expr: Some(type_expr),
        ..
    } = param
    {
        v.visit_type_expr_mut(type_expr);
    }
}

pub fn walk_body_mut<V: VisitorMut + ?Sized>(v: &mut V, body: &mut [Statement]) {
    for statement in body {
        v.visit_statement_mut(statement);
    }
}

pub fn walk_statement_mut<V: VisitorMut + ?Sized>(v: &mut V, statement: &mut Statement) {
    match statement {
        Statement::Expr(expr) => v.visit_expr_mut(expr),
        Statement::Assignment {
            target,
            type_annotation,
            value,
            ..
        } => {
            v.visit_lvalue_mut(target);
            if let Some(type_expr) = type_annotation {
                v.visit_type_expr_mut(type_expr);
            }
            v.visit_expr_mut(value);
        }
        Statement::CompoundAssign { target, value, .. } => {
            v.visit_lvalue_mut(target);
            v.visit_expr_mut(value);
        }
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                v.visit_expr_mut(value);
            }
        }
        Statement::Break { .. } => {}
        Statement::Destructure { pattern, value, .. } => {
            v.visit_pattern_mut(pattern);
            v.visit_expr_mut(value);
        }
    }
}

fn walk_args_mut<V: VisitorMut + ?Sized>(v: &mut V, args: &mut [Arg]) {
    for arg in args {
        v.visit_expr_mut(&mut arg.value);
    }
}

pub fn walk_match_arm_mut<V: VisitorMut + ?Sized>(v: &mut V, arm: &mut MatchArm) {
    v.visit_pattern_mut(&mut arm.pattern);
    if let Some(guard) = &mut arm.guard {
        v.visit_expr_mut(guard);
    }
    walk_body_mut(v, &mut arm.body);
}

fn walk_match_arms_mut<V: VisitorMut + ?Sized>(v: &mut V, arms: &mut [MatchArm]) {
    for arm in arms {
        v.visit_match_arm_mut(arm);
    }
}

fn walk_binary_segments_mut<V: VisitorMut + ?Sized>(v: &mut V, segments: &mut [BinarySegment]) {
    for segment in segments {
        v.visit_expr_mut(&mut segment.value);
        if let Some(size) = &mut segment.size {
            v.visit_expr_mut(size);
        }
        if let Some(type_ann) = &mut segment.type_ann {
            v.visit_type_expr_mut(type_ann);
        }
    }
}

fn walk_field_inits_mut<V: VisitorMut + ?Sized>(v: &mut V, fields: &mut [FieldInit]) {
    for field in fields {
        v.visit_expr_mut(&mut field.value);
    }
}

pub fn walk_expr_mut<V: VisitorMut + ?Sized>(v: &mut V, expr: &mut Expr) {
    match &mut expr.kind {
        ExprKind::Binary { left, right, .. } => {
            v.visit_expr_mut(left);
            v.visit_expr_mut(right);
        }
        ExprKind::BinaryLiteral { segments } => walk_binary_segments_mut(v, segments),
        ExprKind::Call { callee, args, .. } => {
            v.visit_expr_mut(callee);
            walk_args_mut(v, args);
        }
        ExprKind::Closure {
            params,
            return_type,
            body,
        } => {
            for param in params {
                v.visit_closure_param_mut(param);
            }
            if let Some(return_type) = return_type {
                v.visit_type_expr_mut(return_type);
            }
            walk_body_mut(v, body);
        }
        ExprKind::Cond { arms, else_body } => {
            for arm in arms {
                v.visit_expr_mut(&mut arm.condition);
                walk_body_mut(v, &mut arm.body);
            }
            if let Some(else_body) = else_body {
                walk_body_mut(v, else_body);
            }
        }
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Unit => {}
            EnumConstructionData::Tuple(values) => {
                for value in values {
                    v.visit_expr_mut(value);
                }
            }
            EnumConstructionData::Struct(fields) => walk_field_inits_mut(v, fields),
        },
        ExprKind::Fail { value } => v.visit_expr_mut(value),
        ExprKind::FieldAccess { receiver, .. } => v.visit_expr_mut(receiver),
        ExprKind::For {
            pattern,
            iterable,
            body,
        } => {
            v.visit_pattern_mut(pattern);
            v.visit_expr_mut(iterable);
            walk_body_mut(v, body);
        }
        ExprKind::Group { expr } => v.visit_expr_mut(expr),
        ExprKind::Assert {
            condition, message, ..
        } => {
            v.visit_expr_mut(condition);
            if let Some(message) = message {
                v.visit_expr_mut(message);
            }
        }
        ExprKind::Ident { .. } => {}
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            v.visit_expr_mut(condition);
            walk_body_mut(v, then_body);
            if let Some(else_body) = else_body {
                walk_body_mut(v, else_body);
            }
        }
        ExprKind::List { elements } | ExprKind::Tuple { elements } => {
            for element in elements {
                v.visit_expr_mut(element);
            }
        }
        ExprKind::Map { entries } => {
            for (key, value) in entries {
                v.visit_expr_mut(key);
                v.visit_expr_mut(value);
            }
        }
        ExprKind::Literal { .. } => {}
        ExprKind::Loop { body } => walk_body_mut(v, body),
        ExprKind::Match { subject, arms } => {
            v.visit_expr_mut(subject);
            walk_match_arms_mut(v, arms);
        }
        ExprKind::NamedFunctionReference { .. } => {}
        ExprKind::MethodCall { receiver, args, .. } => {
            v.visit_expr_mut(receiver);
            walk_args_mut(v, args);
        }
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            walk_match_arms_mut(v, arms);
            if let Some(timeout) = after_timeout {
                v.visit_expr_mut(timeout);
            }
            walk_body_mut(v, after_body);
        }
        ExprKind::Rescue {
            subject, handler, ..
        } => {
            v.visit_expr_mut(subject);
            v.visit_expr_mut(handler);
        }
        ExprKind::Self_ { .. } => {}
        ExprKind::ShortClosure { params, body } => {
            for param in params {
                v.visit_closure_param_mut(param);
            }
            v.visit_expr_mut(body);
        }
        ExprKind::Spawn { expr } | ExprKind::Try { expr } => v.visit_expr_mut(expr),
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    v.visit_expr_mut(expr);
                }
            }
        }
        ExprKind::StructConstruction { fields, .. } => walk_field_inits_mut(v, fields),
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            v.visit_expr_mut(condition);
            v.visit_expr_mut(then_expr);
            v.visit_expr_mut(else_expr);
        }
        ExprKind::Unary { operand, .. } => v.visit_expr_mut(operand),
        ExprKind::While { condition, body } => {
            v.visit_expr_mut(condition);
            walk_body_mut(v, body);
        }
    }
}

pub fn walk_pattern_mut<V: VisitorMut + ?Sized>(v: &mut V, pattern: &mut Pattern) {
    match pattern {
        Pattern::Wildcard { .. } | Pattern::Literal { .. } | Pattern::Binding { .. } => {}
        Pattern::Binary { segments, .. } => walk_binary_segments_mut(v, segments),
        Pattern::EnumUnit { .. } => {}
        Pattern::EnumTuple { elements, .. }
        | Pattern::Constructor { elements, .. }
        | Pattern::List { elements, .. }
        | Pattern::Tuple { elements, .. } => {
            for element in elements {
                v.visit_pattern_mut(element);
            }
        }
        Pattern::EnumStruct { fields, .. } | Pattern::Struct { fields, .. } => {
            for field in fields {
                v.visit_pattern_mut(&mut field.pattern);
            }
        }
        Pattern::TypedBinding { type_expr, .. } => v.visit_type_expr_mut(type_expr),
        Pattern::Or { patterns, .. } => {
            for pattern in patterns {
                v.visit_pattern_mut(pattern);
            }
        }
    }
}

pub fn walk_type_expr_mut<V: VisitorMut + ?Sized>(v: &mut V, type_expr: &mut TypeExpr) {
    match type_expr {
        TypeExpr::Named { .. } | TypeExpr::Unit { .. } | TypeExpr::Self_ { .. } => {}
        TypeExpr::Generic { args, .. } => {
            for arg in args {
                v.visit_type_expr_mut(arg);
            }
        }
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            for param in params {
                v.visit_type_expr_mut(param);
            }
            v.visit_type_expr_mut(return_type);
        }
        TypeExpr::Tuple { elements, .. }
        | TypeExpr::Union {
            types: elements, ..
        } => {
            for element in elements {
                v.visit_type_expr_mut(element);
            }
        }
    }
}
