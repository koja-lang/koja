//! Resolve-time rewrite for statement-position `for` loops.

use koja_ast::ast::{
    Arg, Diagnostic, Expr, ExprKind, LValue, Literal, MatchArm, Pattern, Statement,
};
use koja_ast::identifier::{Resolution, ResolvedType};
use koja_ast::labels::{pattern_kind_label, pattern_span};
use koja_ast::span::Span;

use super::ctx::Resolver;
use super::expr::resolve_expr;
use super::types::{display_resolution, lookup_type, peel_alias};

/// Rewrite one statement-position `for` into ordinary assignments,
/// `loop`, and `match` nodes. The source expression is resolved first
/// so the rewrite can enforce nominal `Enumeration` conformance.
pub(super) fn rewrite_for_statement(
    statement: Statement,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<Statement>, Box<Statement>> {
    let Statement::Expr(mut expression) = statement else {
        return Err(Box::new(statement));
    };
    let ExprKind::For {
        pattern,
        mut iterable,
        body,
    } = expression.kind
    else {
        return Err(Box::new(Statement::Expr(expression)));
    };

    if let Some(refutable) = first_refutable_pattern(&pattern) {
        diagnostics.push(Diagnostic::error(
            format!(
                "`for` requires an irrefutable pattern. The header contains a {} pattern.",
                pattern_kind_label(refutable),
            ),
            pattern_span(refutable),
        ));
        expression.kind = ExprKind::For {
            pattern,
            iterable,
            body,
        };
        return Err(Box::new(Statement::Expr(expression)));
    }

    resolve_expr(&mut iterable, resolver, diagnostics);
    if !iterable.resolution.is_resolved() {
        expression.kind = ExprKind::For {
            pattern,
            iterable,
            body,
        };
        return Err(Box::new(Statement::Expr(expression)));
    }
    if !implements_enumeration(&iterable.resolution, resolver) {
        diagnostics.push(Diagnostic::error(
            format!(
                "type `{}` in a `for` loop must implement `Enumeration<T, Cursor>`",
                display_resolution(&iterable.resolution, resolver.registry),
            ),
            iterable.span,
        ));
        expression.kind = ExprKind::For {
            pattern,
            iterable,
            body,
        };
        return Err(Box::new(Statement::Expr(expression)));
    }

    let slot = resolver.next_for_slot();
    Ok(build_rewrite(
        pattern,
        *iterable,
        body,
        expression.span,
        slot,
    ))
}

fn implements_enumeration(ty: &ResolvedType, resolver: &Resolver<'_>) -> bool {
    let Some((protocol_id, _)) =
        lookup_type(&["Enumeration".to_string()], resolver.resolution_scope())
    else {
        return false;
    };
    let nominal = peel_alias(ty, resolver.registry);
    match &nominal {
        ResolvedType::Named {
            resolution: Resolution::Global(target_id),
            type_args,
        } => resolver
            .registry
            .lookup_conformance_with(
                *target_id,
                protocol_id,
                type_args,
                resolver.bound_overlay,
                None,
            )
            .is_some(),
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            ..
        } => {
            let slot = index.as_u32() as usize;
            let declared = resolver
                .registry
                .type_param_bounds(*owner)
                .and_then(|bounds| bounds.get(slot))
                .is_some_and(|bounds| bounds.iter().any(|bound| bound.protocol_id == protocol_id));
            declared
                || resolver.bound_overlay.is_some_and(|overlay| {
                    overlay.owner == *owner
                        && overlay.bounds.get(slot).is_some_and(|bounds| {
                            bounds.iter().any(|bound| bound.protocol_id == protocol_id)
                        })
                })
        }
        _ => false,
    }
}

fn first_refutable_pattern(pattern: &Pattern) -> Option<&Pattern> {
    match pattern {
        Pattern::Binding { .. } | Pattern::Wildcard { .. } => None,
        Pattern::Tuple { elements, .. } => elements.iter().find_map(first_refutable_pattern),
        _ => Some(pattern),
    }
}

fn build_rewrite(
    pattern: Pattern,
    iterable: Expr,
    mut body: Vec<Statement>,
    span: Span,
    slot: u32,
) -> Vec<Statement> {
    let source_name = format!("$for_source_{slot}");
    let cursor_name = format!("$for_cursor_{slot}");
    let rest_name = format!("$for_rest_{slot}");

    let source_assignment = assign_local(&source_name, iterable, span);
    let cursor_assignment = assign_local(
        &cursor_name,
        method_call(ident(&source_name, span), "cursor", Vec::new(), span),
        span,
    );

    let mut some_body = vec![assign_local(&cursor_name, ident(&rest_name, span), span)];
    some_body.append(&mut body);
    some_body.push(Statement::Expr(Expr::new(
        ExprKind::Literal {
            value: Literal::Unit,
        },
        span,
    )));

    let some_arm = MatchArm {
        pattern: Pattern::Constructor {
            name: "Some".to_string(),
            elements: vec![Pattern::Tuple {
                elements: vec![
                    pattern,
                    Pattern::Binding {
                        local_id: None,
                        name: rest_name,
                        span,
                    },
                ],
                span,
            }],
            span,
        },
        guard: None,
        body: some_body,
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
    let next = method_call(
        ident(&source_name, span),
        "next",
        vec![Arg {
            name: None,
            value: ident(&cursor_name, span),
            span,
        }],
        span,
    );
    let match_expression = Expr::new(
        ExprKind::Match {
            subject: Box::new(next),
            arms: vec![some_arm, none_arm],
        },
        span,
    );
    let loop_expression = Expr::new(
        ExprKind::Loop {
            body: vec![Statement::Expr(match_expression)],
        },
        span,
    );

    vec![
        source_assignment,
        cursor_assignment,
        Statement::Expr(loop_expression),
    ]
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

fn method_call(receiver: Expr, method: &str, args: Vec<Arg>, span: Span) -> Expr {
    Expr::new(
        ExprKind::MethodCall {
            receiver: Box::new(receiver),
            method: method.to_string(),
            args,
            target: Resolution::Unresolved,
            type_args: Vec::new(),
        },
        span,
    )
}
