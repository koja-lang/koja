//! `for pat in iter ... end` -> desugared `while` shape:
//!
//! ```text
//! __it_n  = <iterable>
//! __len_n = __it_n.length()
//! __idx_n = 0
//! while __idx_n < __len_n
//!   match __it_n.get(__idx_n)
//!     Some(<pat>) -> <surface_body>; __idx_n = __idx_n + 1
//!     None        -> __idx_n = __len_n
//!   end
//! end
//! ```
//!
//! Notes:
//!
//! - `Some` / `None` use [`Pattern::Constructor`] shorthand so
//!   resolve looks the variant up on the subject's enum
//!   (`Global.Option<T>`). A user-defined `Option<T>` in the same
//!   package can't shadow it.
//! - Slot ids are per-function-unique (`__it_0`, `__it_1`, …) so
//!   nested / sibling fors don't collide via the reassignment-
//!   keeps-type rule.
//! - The `None` arm exits via [`Statement::Break`], which the
//!   surrounding `while` resolves under (it pushes a
//!   `loop_break_seen` slot) and IR-lower targets at the
//!   `while_exit` block.
//! - Only statement-position fors are rewritten. Expression-
//!   position fors fall through to resolve's feature-gap diagnostic.

use koja_ast::ast::{Arg, BinOp, Expr, ExprKind, LValue, Literal, MatchArm, Pattern, Statement};
use koja_ast::identifier::Resolution;
use koja_ast::span::Span;

use super::SynthCounter;

/// Recurse into every statement's nested bodies, then splice the
/// desugar in place for any statement-position `for`.
pub(super) fn desugar_body(body: &mut Vec<Statement>, counter: &mut SynthCounter) {
    let mut i = 0;
    while i < body.len() {
        recurse_into_statement(&mut body[i], counter);
        if !matches!(
            &body[i],
            Statement::Expr(Expr {
                kind: ExprKind::For { .. },
                ..
            })
        ) {
            i += 1;
            continue;
        }
        let Statement::Expr(Expr { kind, span, .. }) = body.remove(i) else {
            unreachable!("matches! above guarantees Statement::Expr(For{{..}})");
        };
        let ExprKind::For {
            pattern,
            iterable,
            body: for_body,
        } = kind
        else {
            unreachable!("matches! above guarantees ExprKind::For");
        };
        let synthesized = build_for_desugar(pattern, *iterable, for_body, span, counter);
        let n = synthesized.len();
        for (offset, stmt) in synthesized.into_iter().enumerate() {
            body.insert(i + offset, stmt);
        }
        i += n;
    }
}

fn recurse_into_statement(stmt: &mut Statement, counter: &mut SynthCounter) {
    match stmt {
        Statement::Expr(expr)
        | Statement::Assignment { value: expr, .. }
        | Statement::CompoundAssign { value: expr, .. } => recurse_into_expr(expr, counter),
        Statement::Return {
            value: Some(expr), ..
        } => recurse_into_expr(expr, counter),
        Statement::Return { value: None, .. } | Statement::Break { .. } => {}
    }
}

fn recurse_into_expr(expr: &mut Expr, counter: &mut SynthCounter) {
    match &mut expr.kind {
        ExprKind::Binary { left, right, .. } => {
            recurse_into_expr(left, counter);
            recurse_into_expr(right, counter);
        }
        ExprKind::BinaryLiteral { segments } => {
            for segment in segments.iter_mut() {
                recurse_into_expr(&mut segment.value, counter);
                if let Some(size) = segment.size.as_mut() {
                    recurse_into_expr(size, counter);
                }
            }
        }
        ExprKind::Call { callee, args, .. } => {
            recurse_into_expr(callee, counter);
            for arg in args.iter_mut() {
                recurse_into_expr(&mut arg.value, counter);
            }
        }
        ExprKind::Closure { body, .. } => desugar_body(body, counter),
        ExprKind::Cond { arms, else_body } => {
            for arm in arms.iter_mut() {
                recurse_into_expr(&mut arm.condition, counter);
                desugar_body(&mut arm.body, counter);
            }
            if let Some(else_body) = else_body.as_mut() {
                desugar_body(else_body, counter);
            }
        }
        ExprKind::EnumConstruction { data, .. } => {
            use koja_ast::ast::EnumConstructionData;
            match data {
                EnumConstructionData::Unit => {}
                EnumConstructionData::Tuple(elements) => {
                    for elem in elements.iter_mut() {
                        recurse_into_expr(elem, counter);
                    }
                }
                EnumConstructionData::Struct(fields) => {
                    for field in fields.iter_mut() {
                        recurse_into_expr(&mut field.value, counter);
                    }
                }
            }
        }
        ExprKind::FieldAccess { receiver, .. } => recurse_into_expr(receiver, counter),
        ExprKind::For { iterable, body, .. } => {
            recurse_into_expr(iterable, counter);
            desugar_body(body, counter);
        }
        ExprKind::Group { expr } => recurse_into_expr(expr, counter),
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            recurse_into_expr(condition, counter);
            desugar_body(then_body, counter);
            if let Some(else_body) = else_body.as_mut() {
                desugar_body(else_body, counter);
            }
        }
        ExprKind::List { elements } => {
            for elem in elements.iter_mut() {
                recurse_into_expr(elem, counter);
            }
        }
        ExprKind::Loop { body } => desugar_body(body, counter),
        ExprKind::Map { entries } => {
            for (k, v) in entries.iter_mut() {
                recurse_into_expr(k, counter);
                recurse_into_expr(v, counter);
            }
        }
        ExprKind::Match { subject, arms } => {
            recurse_into_expr(subject, counter);
            for arm in arms.iter_mut() {
                if let Some(guard) = arm.guard.as_mut() {
                    recurse_into_expr(guard, counter);
                }
                desugar_body(&mut arm.body, counter);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            recurse_into_expr(receiver, counter);
            for arg in args.iter_mut() {
                recurse_into_expr(&mut arg.value, counter);
            }
        }
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            for arm in arms.iter_mut() {
                if let Some(guard) = arm.guard.as_mut() {
                    recurse_into_expr(guard, counter);
                }
                desugar_body(&mut arm.body, counter);
            }
            if let Some(timeout) = after_timeout.as_mut() {
                recurse_into_expr(timeout, counter);
            }
            desugar_body(after_body, counter);
        }
        ExprKind::ShortClosure { body, .. } => recurse_into_expr(body, counter),
        ExprKind::Spawn { expr } => recurse_into_expr(expr, counter),
        ExprKind::String { parts, .. } => {
            use koja_ast::ast::StringPart;
            for part in parts.iter_mut() {
                if let StringPart::Interpolation { expr, .. } = part {
                    recurse_into_expr(expr, counter);
                }
            }
        }
        ExprKind::StructConstruction { fields, .. } => {
            for field in fields.iter_mut() {
                recurse_into_expr(&mut field.value, counter);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            recurse_into_expr(condition, counter);
            recurse_into_expr(then_expr, counter);
            recurse_into_expr(else_expr, counter);
        }
        ExprKind::Unary { operand, .. } => recurse_into_expr(operand, counter),
        ExprKind::Unless { condition, body } => {
            recurse_into_expr(condition, counter);
            desugar_body(body, counter);
        }
        ExprKind::While { condition, body } => {
            recurse_into_expr(condition, counter);
            desugar_body(body, counter);
        }
        ExprKind::Ident { .. } | ExprKind::Literal { .. } | ExprKind::Self_ { .. } => {}
    }
}

fn build_for_desugar(
    pattern: Pattern,
    iterable: Expr,
    for_body: Vec<Statement>,
    span: Span,
    counter: &mut SynthCounter,
) -> Vec<Statement> {
    let slot = counter.next();
    let it_name = format!("__it_{slot}");
    let len_name = format!("__len_{slot}");
    let idx_name = format!("__idx_{slot}");

    let init_it = assign_local(&it_name, iterable, span);
    let init_len = assign_local(
        &len_name,
        method_call(ident(&it_name, span), "length", Vec::new(), span),
        span,
    );
    let init_idx = assign_local(&idx_name, int_literal("0", span), span);

    let while_condition = binary_op(
        BinOp::Lt,
        ident(&idx_name, span),
        ident(&len_name, span),
        span,
    );

    let some_arm = MatchArm {
        pattern: Pattern::Constructor {
            name: "Some".to_string(),
            elements: vec![pattern],
            span,
        },
        guard: None,
        body: append_increment(for_body, &idx_name, span),
        span,
    };
    let none_arm = MatchArm {
        pattern: Pattern::Constructor {
            name: "None".to_string(),
            elements: Vec::new(),
            span,
        },
        guard: None,
        body: vec![Statement::Break { span }],
        span,
    };
    let match_subject = method_call(
        ident(&it_name, span),
        "get",
        vec![Arg {
            name: None,
            value: ident(&idx_name, span),
            span,
        }],
        span,
    );
    let match_expr = Expr::new(
        ExprKind::Match {
            subject: Box::new(match_subject),
            arms: vec![some_arm, none_arm],
        },
        span,
    );
    let while_body = vec![Statement::Expr(match_expr)];
    let while_expr = Expr::new(
        ExprKind::While {
            condition: Box::new(while_condition),
            body: while_body,
        },
        span,
    );

    vec![init_it, init_len, init_idx, Statement::Expr(while_expr)]
}

/// Append `__idx_n = __idx_n + 1`. Tail position so an early
/// `return` inside the surface body skips the increment.
fn append_increment(mut body: Vec<Statement>, idx_name: &str, span: Span) -> Vec<Statement> {
    let increment = binary_op(
        BinOp::Add,
        ident(idx_name, span),
        int_literal("1", span),
        span,
    );
    body.push(assign_local(idx_name, increment, span));
    body
}

fn assign_local(name: &str, value: Expr, span: Span) -> Statement {
    Statement::Assignment {
        target: LValue {
            head_resolved_type: None,
            local_id: None,
            segments: vec![name.to_string()],
            span,
        },
        type_annotation: None,
        value,
        span,
    }
}

fn ident(name: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::Ident {
            name: name.to_string(),
            resolution: Resolution::Unresolved,
        },
        span,
    )
}

fn int_literal(digits: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::Literal {
            value: Literal::Int(digits.to_string()),
        },
        span,
    )
}

fn binary_op(op: BinOp, left: Expr, right: Expr, span: Span) -> Expr {
    Expr::new(
        ExprKind::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        },
        span,
    )
}

fn method_call(receiver: Expr, method: &str, args: Vec<Arg>, span: Span) -> Expr {
    Expr::new(
        ExprKind::MethodCall {
            receiver: Box::new(receiver),
            method: method.to_string(),
            args,
            type_args: Vec::new(),
        },
        span,
    )
}
