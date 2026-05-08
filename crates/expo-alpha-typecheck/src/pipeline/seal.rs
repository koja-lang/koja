//! Seal sub-pass: assert every relevant [`Resolution`] /
//! [`expo_ast::identifier::ResolvedType`] annotation is populated.
//! Panics on violation per [`COMPILER-NORTHSTAR.md`] — seal failures
//! are upstream compiler bugs, not user errors.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../../design/COMPILER-NORTHSTAR.md

use expo_ast::ast::{
    AssignTarget, Constant, EnumConstructionData, Expr, ExprKind, File, Function, ImplMember, Item,
    LValue, Pattern, Statement, StringPart, TypeExpr,
};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;

use crate::program::CheckedProgram;
use crate::registry::{GlobalKind, GlobalRegistry};
use expo_ast::labels::{expr_kind_label, pattern_kind_label, pattern_span};

/// Asserts the sealed-AST invariants on `program`. Panics on violation.
///
/// Generic decl bodies (functions with their own type params, plus
/// inline `fn` items on generic struct/enum decls and impl-block
/// methods on generic targets) are skipped — their bodies still
/// carry [`Resolution::TypeParam`] leaves until IR's monomorphization
/// substitutes through and re-lowers a concrete copy. The IR pipeline
/// drops generic templates before seal-equivalent invariants apply
/// downstream.
pub(crate) fn seal_ast(program: &CheckedProgram) {
    for pkg in &program.packages {
        for file in &pkg.files {
            seal_file(file, &pkg.package, &program.registry);
        }
    }
}

fn seal_file(file: &File, package: &str, registry: &GlobalRegistry) {
    for item in &file.items {
        match item {
            Item::Function(function) => {
                if !function.type_params.is_empty() {
                    continue;
                }
                seal_function(function);
            }
            Item::Struct(decl) => {
                let owner_generic = !decl.type_params.is_empty();
                for function in &decl.functions {
                    if owner_generic || !function.type_params.is_empty() {
                        continue;
                    }
                    seal_function(function);
                }
            }
            Item::Enum(decl) => {
                let owner_generic = !decl.type_params.is_empty();
                for function in &decl.functions {
                    if owner_generic || !function.type_params.is_empty() {
                        continue;
                    }
                    seal_function(function);
                }
            }
            Item::Impl(impl_block) => {
                let target_generic = impl_target_is_generic(&impl_block.target, package, registry);
                for member in &impl_block.members {
                    if let ImplMember::Function(function) = member
                        && function.type_params.is_empty()
                        && !target_generic
                    {
                        seal_function(function);
                    }
                }
            }
            Item::Constant(constant) => {
                seal_constant(constant, package, registry);
            }
            _ => {}
        }
    }
    if let Some(body) = file.body.as_ref() {
        // Script-mode files keep their top-level statements on
        // `file.body`; downstream passes consume them directly. Seal
        // the same statement-tree invariants function bodies satisfy.
        for stmt in body {
            seal_statement(stmt);
        }
    }
}

/// True when an `impl` target names a generic struct/enum (e.g.
/// `impl Pair` or `impl Show for List<T>`). Methods on a generic
/// target inherit the type-param scope (struct's slot anchors for
/// inherent impls, the impl entry's free-name anchors for
/// `impl Trait for Type<T>`), so their bodies carry `TypeParam`
/// resolutions and seal must skip them.
fn impl_target_is_generic(target: &TypeExpr, package: &str, registry: &GlobalRegistry) -> bool {
    let path = match target {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => path,
        _ => return false,
    };
    if path.len() != 1 {
        return false;
    }
    let identifier = Identifier::new(package, vec![path[0].clone()]);
    registry
        .lookup(&identifier)
        .is_some_and(|(_, entry)| !entry.type_params.is_empty())
}

/// Assert lift's constants pass produced a stamped
/// [`crate::registry::ConstantDefinition`], then seal the value
/// expression like any other resolved expression. The body shape is
/// already constrained to literals + struct/enum-of-literals, so the
/// reused `seal_expr` walk is sufficient.
fn seal_constant(constant: &Constant, package: &str, registry: &GlobalRegistry) {
    let identifier = Identifier::new(package, vec![constant.name.clone()]);
    let Some((_, entry)) = registry.lookup(&identifier) else {
        seal_panic(
            &format!(
                "constant `{identifier}` missing from registry — collect/lift invariant violation",
            ),
            constant.span,
        );
    };
    match &entry.kind {
        GlobalKind::Constant(Some(_)) => {}
        GlobalKind::Constant(None) => seal_panic(
            &format!(
                "constant `{identifier}` reached seal without a stamped definition — \
                 lift_signatures::constants invariant violation",
            ),
            constant.span,
        ),
        other => seal_panic(
            &format!(
                "registry entry for `{identifier}` is `{}`, expected `constant` — \
                 collect/lift invariant violation",
                other.label(),
            ),
            constant.span,
        ),
    }
    seal_expr(&constant.value);
}

fn seal_function(function: &Function) {
    let Some(body) = function.body.as_ref() else {
        return;
    };
    for stmt in body {
        seal_statement(stmt);
    }
}

fn seal_statement(stmt: &Statement) {
    match stmt {
        Statement::Assignment {
            span,
            target,
            value,
            ..
        } => {
            seal_assign_target(target, *span);
            seal_expr(value);
        }
        Statement::Break { .. } | Statement::Return { value: None, .. } => {}
        Statement::CompoundAssign {
            target,
            value,
            span,
            ..
        } => {
            seal_compound_target(target, *span);
            seal_expr(value);
        }
        Statement::Expr(expr) => seal_expr(expr),
        Statement::Return {
            value: Some(value), ..
        } => seal_expr(value),
    }
}

/// Assignment targets must be single-segment [`AssignTarget::LValue`]s
/// — the resolver rejected pattern destructuring and dotted lvalues
/// upstream, so reaching seal with anything else is a compiler bug.
fn seal_assign_target(target: &AssignTarget, statement_span: Span) {
    match target {
        AssignTarget::LValue(lvalue) => {
            if lvalue.segments.len() != 1 {
                seal_panic(
                    &format!(
                        "assignment target has {} segments; resolver rejects multi-segment \
                         targets",
                        lvalue.segments.len(),
                    ),
                    lvalue.span,
                );
            }
        }
        AssignTarget::Pattern(_) => seal_panic(
            "assignment target is a destructuring pattern; resolver rejects this shape",
            statement_span,
        ),
    }
}

/// Compound-assign targets are bare `LValue`s (the AST shape only
/// admits the single-segment case as a happy-path; the resolver
/// rejects multi-segment forms and undeclared names). Past resolve,
/// a compound-assign target must carry both single-segment shape
/// *and* a stamped `local_id`.
fn seal_compound_target(target: &LValue, statement_span: Span) {
    if target.segments.len() != 1 {
        seal_panic(
            &format!(
                "compound-assign target has {} segments; resolver rejects multi-segment \
                 targets",
                target.segments.len(),
            ),
            target.span,
        );
    }
    if target.local_id.is_none() {
        seal_panic(
            &format!(
                "compound-assign target `{}` carries no LocalId; resolver should have \
                 stamped it on success or diagnosed otherwise",
                target.segments[0],
            ),
            statement_span,
        );
    }
}

fn seal_expr(expr: &Expr) {
    // The callee position of a `Call` is the one carve-out: function
    // names aren't first-class values yet, so the outer callee
    // `Expr.resolution` stays `Unresolved`. Every other position must
    // carry a fully-resolved type that doesn't leak `TypeParam` —
    // those are decl-side annotations and have no business on a
    // construction-site value.
    if !expr.resolution.is_resolved() {
        seal_panic("expression missing resolution", expr.span);
    }
    seal_no_type_param(&expr.resolution, expr.span);
    match &expr.kind {
        ExprKind::Binary { left, right, .. } => {
            seal_expr(left);
            seal_expr(right);
        }
        ExprKind::Call {
            callee,
            args,
            type_args,
        } => {
            seal_call_callee(callee);
            for arg in args {
                seal_expr(&arg.value);
            }
            for ty in type_args {
                seal_no_type_param(ty, expr.span);
            }
        }
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Struct(fields) => {
                for field in fields {
                    seal_expr(&field.value);
                }
            }
            EnumConstructionData::Tuple(exprs) => {
                for expr in exprs {
                    seal_expr(expr);
                }
            }
            EnumConstructionData::Unit => {}
        },
        ExprKind::FieldAccess { receiver, .. } => seal_expr(receiver),
        ExprKind::Group { expr: inner } => seal_expr(inner),
        ExprKind::Ident { name, resolution } => {
            // `Resolution::Global` (struct names, callees) and
            // `Resolution::Local` (param/local references) satisfy
            // seal. `Resolution::Unresolved` and a leaked
            // `Resolution::TypeParam` are both compiler bugs.
            match resolution {
                Resolution::Global(_) | Resolution::Local(_) => {}
                Resolution::TypeParam { .. } => seal_panic(
                    &format!("identifier `{name}` resolves to a TypeParam after typecheck"),
                    expr.span,
                ),
                Resolution::Unresolved => seal_panic(
                    &format!("identifier `{name}` has Unresolved resolution after typecheck"),
                    expr.span,
                ),
            }
        }
        ExprKind::Cond { arms, else_body } => {
            for arm in arms {
                seal_expr(&arm.condition);
                for stmt in &arm.body {
                    seal_statement(stmt);
                }
            }
            if let Some(else_body) = else_body {
                for stmt in else_body {
                    seal_statement(stmt);
                }
            }
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            seal_expr(condition);
            for stmt in then_body {
                seal_statement(stmt);
            }
            if let Some(else_body) = else_body {
                for stmt in else_body {
                    seal_statement(stmt);
                }
            }
        }
        ExprKind::Literal { .. } => {}
        ExprKind::Match { subject, arms } => {
            seal_expr(subject);
            for arm in arms {
                seal_pattern(&arm.pattern);
                for stmt in &arm.body {
                    seal_statement(stmt);
                }
            }
        }
        ExprKind::Self_ { .. } => {}
        ExprKind::MethodCall {
            receiver,
            args,
            type_args,
            ..
        } => {
            // Static method calls: receiver must resolve like any
            // other `Ident` reference (its `resolution` is the
            // struct id, populated by resolve). Args follow the same
            // rule as `Call`. The outer `Expr.resolution` is the
            // method's return type, already enforced by the
            // top-of-fn check.
            seal_expr(receiver);
            for arg in args {
                seal_expr(&arg.value);
            }
            for ty in type_args {
                seal_no_type_param(ty, expr.span);
            }
        }
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    seal_expr(expr);
                }
            }
        }
        ExprKind::StructConstruction { fields, .. } => {
            for field in fields {
                seal_expr(&field.value);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            seal_expr(condition);
            seal_expr(then_expr);
            seal_expr(else_expr);
        }
        ExprKind::Unary { operand, .. } => seal_expr(operand),
        ExprKind::Unless { condition, body } => {
            seal_expr(condition);
            for stmt in body {
                seal_statement(stmt);
            }
        }
        other => seal_panic(
            &format!(
                "alpha typecheck seal does not yet recognize expression kind `{}`",
                expr_kind_label(other)
            ),
            expr.span,
        ),
    }
}

/// Seal the callee of a `Call`: the outer `Expr.resolution` stays
/// `Unresolved` (function names aren't values yet); we check the inner
/// `Ident` carries a `Global(_)` resolution so IR lowering has a
/// concrete target.
fn seal_call_callee(callee: &Expr) {
    let ExprKind::Ident { name, resolution } = &callee.kind else {
        seal_panic(
            &format!(
                "call site has a non-identifier callee `{}` that passed typecheck",
                expr_kind_label(&callee.kind),
            ),
            callee.span,
        );
    };
    if matches!(resolution, Resolution::Unresolved) {
        seal_panic(
            &format!("callee `{name}` has Unresolved resolution after typecheck"),
            callee.span,
        );
    }
}

/// Walk `ty` and assert no `Resolution::TypeParam` leaf escapes into
/// runtime-value position. Concrete `type_args` are fine — and
/// expected for monomorphizable construction sites — so this only
/// rejects the `TypeParam` head.
fn seal_no_type_param(ty: &ResolvedType, span: Span) {
    if let Resolution::TypeParam { owner, index } = ty.resolution {
        seal_panic(
            &format!(
                "ResolvedType leaf carries TypeParam {{ owner: {owner}, index: {index} }} \
                 outside a generic-decl body",
            ),
            span,
        );
    }
    for arg in &ty.type_args {
        seal_no_type_param(arg, span);
    }
}

/// Supported patterns are leaves: wildcards, literals, and bindings
/// (which must carry a stamped `LocalId`). Every other shape is a
/// feature-gap diagnostic in resolve and never reaches seal on the
/// success path; if one slips through, that is an upstream bug.
fn seal_pattern(pattern: &Pattern) {
    match pattern {
        Pattern::Binding {
            local_id,
            name,
            span,
        } => {
            if local_id.is_none() {
                seal_panic(
                    &format!(
                        "match binding `{name}` carries no LocalId; resolver should have \
                         stamped it on the success path",
                    ),
                    *span,
                );
            }
        }
        Pattern::EnumTuple {
            elements,
            type_path,
            variant,
            span,
            ..
        } => {
            seal_enum_path(type_path, variant, *span);
            for element in elements {
                seal_pattern(element);
            }
        }
        Pattern::EnumUnit {
            type_path,
            variant,
            span,
            ..
        } => seal_enum_path(type_path, variant, *span),
        Pattern::Or { patterns, span } => {
            if patterns.is_empty() {
                seal_panic("or-pattern carries no alternatives", *span);
            }
            for alternative in patterns {
                seal_pattern(alternative);
            }
        }
        Pattern::Literal { .. } | Pattern::Wildcard { .. } => {}
        other => seal_panic(
            &format!(
                "alpha typecheck seal does not yet recognize pattern kind `{}`",
                pattern_kind_label(other),
            ),
            pattern_span(other),
        ),
    }
}

fn seal_enum_path(type_path: &[String], variant: &str, span: Span) {
    if type_path.is_empty() {
        seal_panic(
            &format!("enum pattern `{variant}` carries an empty type path"),
            span,
        );
    }
    if variant.is_empty() {
        seal_panic("enum pattern carries an empty variant name", span);
    }
}

fn seal_panic(message: &str, span: Span) -> ! {
    panic!(
        "alpha typecheck seal violation: {message} at line {}, column {}",
        span.start.line, span.start.column
    );
}
