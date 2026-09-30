//! Post-resolve position check for `CPtr.borrow`. The borrowed
//! pointer is a zero-cost view of a `Binary`'s payload, valid only
//! while the source is live. Consuming it within the borrowing
//! statement (call argument, chained receiver) is always safe under
//! ordinary scope-exit drop semantics, so those are the only legal
//! positions. Binding, returning, or storing the result is rejected
//! with a teaching diagnostic pointing at `CPtr.copy`.

use koja_ast::ast::{
    Diagnostic, EnumConstructionData, Expr, ExprKind, File, Function, ImplMember, Item, LValue,
    Statement, StringPart, path_text,
};
use koja_ast::identifier::Resolution;

use crate::registry::GlobalRegistry;

/// How the expression position under inspection treats a
/// `CPtr.borrow` result.
#[derive(Clone, Copy)]
enum Position<'a> {
    /// Right-hand side of `target = ...`.
    Bound(&'a LValue),
    /// Consumed in-statement as a call argument or chained receiver.
    Consumed,
    /// Any other position (struct field, collection element, ...).
    Escaping,
    /// Explicit `return` or implicit tail-expression return.
    Returned,
}

pub(crate) fn check_file(
    file: &File,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut walker = Walker {
        diagnostics,
        registry,
    };
    for item in &file.items {
        walker.check_item(item);
    }
    if let Some(body) = file.body.as_ref() {
        walker.check_body(body, Position::Escaping);
    }
}

/// Recursion state for one file. `registry` answers the
/// `CPtr.borrow` identity check and `diagnostics` collects the
/// escapes.
struct Walker<'a> {
    diagnostics: &'a mut Vec<Diagnostic>,
    registry: &'a GlobalRegistry,
}

impl Walker<'_> {
    fn check_item(&mut self, item: &Item) {
        match item {
            Item::Builtin(decl) => self.check_functions(&decl.functions),
            Item::Enum(decl) => self.check_functions(&decl.functions),
            Item::Extend(block) => self.check_members(&block.members),
            Item::Function(function) => self.check_function(function),
            Item::Impl(block) => self.check_members(&block.members),
            Item::Struct(decl) => self.check_functions(&decl.functions),
            _ => {}
        }
    }

    fn check_functions(&mut self, functions: &[Function]) {
        for function in functions {
            self.check_function(function);
        }
    }

    fn check_members(&mut self, members: &[ImplMember]) {
        for member in members {
            if let ImplMember::Function(function) = member {
                self.check_function(function);
            }
        }
    }

    fn check_function(&mut self, function: &Function) {
        if let Some(body) = function.body.as_ref() {
            self.check_body(body, Position::Returned);
        }
    }

    /// Walk a statement body. The tail statement, when it is a bare
    /// expression, produces the body's value, so it checks against
    /// `tail` (implicit return for function/closure bodies) instead of
    /// the ordinary statement positions.
    fn check_body(&mut self, body: &[Statement], tail: Position<'_>) {
        let Some((last, leading)) = body.split_last() else {
            return;
        };
        for stmt in leading {
            self.check_statement(stmt);
        }
        match last {
            Statement::Expr(expr) => self.check_expr(expr, tail),
            other => self.check_statement(other),
        }
    }

    fn check_statement(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Assignment { target, value, .. } => {
                self.check_expr(value, Position::Bound(target));
            }
            Statement::Break { .. } | Statement::Return { value: None, .. } => {}
            Statement::CompoundAssign { value, .. } => {
                self.check_expr(value, Position::Escaping);
            }
            Statement::Destructure { value, .. } => {
                self.check_expr(value, Position::Escaping);
            }
            Statement::Expr(expr) => self.check_expr(expr, Position::Escaping),
            Statement::Return {
                value: Some(value), ..
            } => self.check_expr(value, Position::Returned),
        }
    }

    fn check_expr(&mut self, expr: &Expr, position: Position<'_>) {
        if is_cptr_borrow(expr, self.registry) {
            self.emit_escape(position, expr);
        }
        match &expr.kind {
            // Only an `assert` that failed its channel check survives
            // resolve. Walk it so the operands still get checked.
            ExprKind::Assert {
                condition, message, ..
            } => {
                self.check_expr(condition, Position::Consumed);
                if let Some(message) = message {
                    self.check_expr(message, Position::Consumed);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.check_expr(left, Position::Escaping);
                self.check_expr(right, Position::Escaping);
            }
            ExprKind::BinaryLiteral { segments } => {
                for segment in segments {
                    self.check_expr(&segment.value, Position::Escaping);
                    if let Some(size) = segment.size.as_ref() {
                        self.check_expr(size, Position::Escaping);
                    }
                }
            }
            ExprKind::Call { args, .. } => {
                for arg in args {
                    self.check_expr(&arg.value, Position::Consumed);
                }
            }
            ExprKind::Closure { body, .. } => {
                self.check_body(body, Position::Returned);
            }
            ExprKind::Cond { arms, else_body } => {
                for arm in arms {
                    self.check_expr(&arm.condition, Position::Escaping);
                    self.check_body(&arm.body, Position::Escaping);
                }
                if let Some(else_body) = else_body {
                    self.check_body(else_body, Position::Escaping);
                }
            }
            ExprKind::EnumConstruction { data, .. } => match data {
                EnumConstructionData::Struct(fields) => {
                    for field in fields {
                        self.check_expr(&field.value, Position::Escaping);
                    }
                }
                EnumConstructionData::Tuple(exprs) => {
                    for expr in exprs {
                        self.check_expr(expr, Position::Escaping);
                    }
                }
                EnumConstructionData::Unit => {}
            },
            ExprKind::Fail { value } => {
                self.check_expr(value, Position::Returned);
            }
            ExprKind::FieldAccess { receiver, .. } => {
                self.check_expr(receiver, Position::Escaping);
            }
            ExprKind::For { iterable, body, .. } => {
                self.check_expr(iterable, Position::Escaping);
                self.check_body(body, Position::Escaping);
            }
            // Parentheses are pure grouping, so `(CPtr.borrow(b)).read()`
            // consumes the same as the unparenthesized chain.
            ExprKind::Group { expr: inner } => self.check_expr(inner, position),
            ExprKind::Ident { .. }
            | ExprKind::Literal { .. }
            | ExprKind::NamedFunctionReference { .. }
            | ExprKind::Self_ { .. } => {}
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                self.check_expr(condition, Position::Escaping);
                self.check_body(then_body, Position::Escaping);
                if let Some(else_body) = else_body {
                    self.check_body(else_body, Position::Escaping);
                }
            }
            ExprKind::List { elements } => {
                for element in elements {
                    self.check_expr(element, Position::Escaping);
                }
            }
            ExprKind::Map { entries } => {
                for (key, value) in entries {
                    self.check_expr(key, Position::Escaping);
                    self.check_expr(value, Position::Escaping);
                }
            }
            ExprKind::Loop { body } => self.check_body(body, Position::Escaping),
            ExprKind::Match { subject, arms } => {
                self.check_expr(subject, Position::Escaping);
                for arm in arms {
                    if let Some(guard) = arm.guard.as_ref() {
                        self.check_expr(guard, Position::Escaping);
                    }
                    self.check_body(&arm.body, Position::Escaping);
                }
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                self.check_expr(receiver, Position::Consumed);
                for arg in args {
                    self.check_expr(&arg.value, Position::Consumed);
                }
            }
            ExprKind::Receive {
                arms,
                after_timeout,
                after_body,
            } => {
                for arm in arms {
                    if let Some(guard) = arm.guard.as_ref() {
                        self.check_expr(guard, Position::Escaping);
                    }
                    self.check_body(&arm.body, Position::Escaping);
                }
                if let Some(timeout) = after_timeout.as_ref() {
                    self.check_expr(timeout, Position::Escaping);
                }
                self.check_body(after_body, Position::Escaping);
            }
            ExprKind::Rescue {
                subject, handler, ..
            } => {
                self.check_expr(subject, Position::Escaping);
                self.check_expr(handler, Position::Escaping);
            }
            ExprKind::ShortClosure { body, .. } => {
                self.check_expr(body, Position::Returned);
            }
            ExprKind::Spawn { expr: inner } => {
                self.check_expr(inner, Position::Escaping);
            }
            ExprKind::String { parts, .. } => {
                for part in parts {
                    if let StringPart::Interpolation { expr: inner, .. } = part {
                        self.check_expr(inner, Position::Escaping);
                    }
                }
            }
            ExprKind::StructConstruction { fields, .. } => {
                for field in fields {
                    self.check_expr(&field.value, Position::Escaping);
                }
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.check_expr(condition, Position::Escaping);
                self.check_expr(then_expr, Position::Escaping);
                self.check_expr(else_expr, Position::Escaping);
            }
            ExprKind::Try { expr: inner } => {
                self.check_expr(inner, Position::Escaping);
            }
            ExprKind::Tuple { elements } => {
                for element in elements {
                    self.check_expr(element, Position::Escaping);
                }
            }
            ExprKind::Unary { operand, .. } => {
                self.check_expr(operand, Position::Escaping);
            }
            ExprKind::While { condition, body } => {
                self.check_expr(condition, Position::Escaping);
                self.check_body(body, Position::Escaping);
            }
        }
    }

    fn emit_escape(&mut self, position: Position<'_>, expr: &Expr) {
        let opening = match position {
            Position::Bound(target) => {
                format!(
                    "a borrowed pointer cannot be bound to `{}`",
                    path_text(&target.segments),
                )
            }
            Position::Consumed => return,
            Position::Escaping => "a borrowed pointer cannot be stored".to_string(),
            Position::Returned => "a borrowed pointer cannot be returned".to_string(),
        };
        self.diagnostics.push(Diagnostic::error(
            format!(
                "{opening}. It is only valid within the statement that borrows it. Pass it \
             directly to a call, or use `CPtr.copy(...)` for an owned copy",
            ),
            expr.span,
        ));
    }
}

/// True when `expr` is a static call to the `Global.CPtr.borrow`
/// intrinsic. Resolve rewrites static receivers to a synthetic
/// `Ident` carrying the type's `Resolution::Global`, so the match is
/// exact even through aliasing or local shadowing.
fn is_cptr_borrow(expr: &Expr, registry: &GlobalRegistry) -> bool {
    let ExprKind::MethodCall {
        receiver, method, ..
    } = &expr.kind
    else {
        return false;
    };
    if method != "borrow" {
        return false;
    }
    let ExprKind::Ident {
        resolution: Resolution::Global(receiver_id),
        ..
    } = &receiver.kind
    else {
        return false;
    };
    registry.get(*receiver_id).is_some_and(|entry| {
        entry.identifier.is_in_package("Global") && entry.identifier.path() == ["CPtr"]
    })
}
