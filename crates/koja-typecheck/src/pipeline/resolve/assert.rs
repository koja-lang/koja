//! Statement-position `assert` desugaring.
//!
//! `assert cond, message` becomes ordinary AST that the walker then
//! resolves like hand-written code:
//!
//! ```text
//! $assert_left_N = a            # comparison forms only
//! $assert_right_N = b
//! if not ($assert_left_N == $assert_right_N)
//!   fail Test.Failure.Assertion(Test.Assertion{
//!     expression: "a == b", source_line: "...", file: "...",
//!     line: L, column: C,
//!     left: Option.Some($assert_left_N.format()),
//!     right: Option.Some($assert_right_N.format()),
//!     message: Option.Some(message),
//!   })
//! end
//! ```
//!
//! The inner `fail` goes through [`super::error_channel`] unchanged,
//! so `Result.Err` wrapping stays in one place. The only check that
//! lives here is the channel rule: the enclosing function must
//! declare `! Test.Failure`, which application code cannot name
//! because the loader links the `Test` package only with tests.
//!
//! Binding the operands apart would type `b` with no context, so
//! `assert n == 0` on a `UInt32` and `assert x == Option.None` would
//! fail where the same `if` passes. The walker closes that gap: it
//! resolves the left binding first and hands its type to the right
//! binding as the expected type, the way `==` types its operands.

use koja_ast::ast::{
    AssertSource, BinOp, Diagnostic, EnumConstructionData, Expr, ExprKind, FieldInit, Literal,
    Name, Statement, StringPart, UnaryOp,
};
use koja_ast::identifier::{Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;

use super::ctx::Resolver;
use super::for_loop::{assign_local, ident, method_call};
use super::types::peel_alias;

const TEST_PACKAGE: &str = "Test";
const FAILURE_TYPE: &str = "Failure";
const ASSERTION_TYPE: &str = "Assertion";

pub(super) fn is_assert_statement(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::Expr(Expr {
            kind: ExprKind::Assert { .. },
            ..
        })
    )
}

/// The statements one `assert` desugars to.
pub(super) struct AssertRewrite {
    pub(super) statements: Vec<Statement>,
    /// The left temporary's name when the condition was a comparison.
    /// Then `statements[0]` binds it and `statements[1]` binds the
    /// right operand, which the walker resolves with the left's type
    /// as the expected type.
    pub(super) comparison_left: Option<String>,
}

impl AssertRewrite {
    fn plain(statements: Vec<Statement>) -> Self {
        Self {
            statements,
            comparison_left: None,
        }
    }
}

/// Rewrite one statement-position `assert` into the statements above.
/// On a channel error the diagnostic is reported and only the
/// condition comes back, so it still resolves for further
/// diagnostics without a second `assert` error from expression
/// dispatch.
pub(super) fn rewrite_assert_statement(
    statement: Statement,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> AssertRewrite {
    let Statement::Expr(expression) = statement else {
        return AssertRewrite::plain(vec![statement]);
    };
    let span = expression.span;
    let ExprKind::Assert {
        condition,
        message,
        source,
    } = expression.kind
    else {
        return AssertRewrite::plain(vec![Statement::Expr(expression)]);
    };

    if !channel_is_test_failure(resolver) {
        diagnostics.push(channel_diagnostic(resolver, span));
        return AssertRewrite::plain(vec![Statement::Expr(*condition)]);
    }

    let condition_span = condition.span;
    let slot = resolver.next_assert_slot();
    let mut statements = Vec::with_capacity(3);
    let mut comparison_left = None;
    let (condition, left, right) = match comparison_operands(condition) {
        Ok((op, left, right)) => {
            let left_name = format!("$assert_left_{slot}");
            let right_name = format!("$assert_right_{slot}");
            statements.push(assign_local(&left_name, left, span));
            statements.push(assign_local(&right_name, right, span));
            comparison_left = Some(left_name.clone());
            let rebuilt = Expr::new(
                ExprKind::Binary {
                    op,
                    left: Box::new(ident(&left_name, condition_span)),
                    right: Box::new(ident(&right_name, condition_span)),
                },
                condition_span,
            );
            (
                rebuilt,
                some(format_call(&left_name, span), span),
                some(format_call(&right_name, span), span),
            )
        }
        Err(condition) => (*condition, none(span), none(span)),
    };

    let message = match message {
        Some(message) => some(*message, span),
        None => none(span),
    };
    let assertion = assertion_construction(&source, condition_span, left, right, message, span);
    let failure = Expr::new(
        ExprKind::EnumConstruction {
            type_path: vec![Name::new(TEST_PACKAGE, span), Name::new(FAILURE_TYPE, span)],
            variant: Name::new(ASSERTION_TYPE, span),
            data: EnumConstructionData::Tuple(vec![assertion]),
        },
        span,
    );
    let fail = Statement::Expr(Expr::new(
        ExprKind::Fail {
            value: Box::new(failure),
        },
        span,
    ));
    let negated = Expr::new(
        ExprKind::Unary {
            op: UnaryOp::Not,
            operand: Box::new(Expr::new(
                ExprKind::Group {
                    expr: Box::new(condition),
                },
                condition_span,
            )),
        },
        condition_span,
    );
    statements.push(Statement::Expr(Expr::new(
        ExprKind::If {
            condition: Box::new(negated),
            then_body: vec![fail],
            else_body: None,
        },
        span,
    )));
    AssertRewrite {
        statements,
        comparison_left,
    }
}

/// Split a comparison condition into its operator and operands. Any
/// other shape comes back whole.
fn comparison_operands(condition: Box<Expr>) -> Result<(BinOp, Expr, Expr), Box<Expr>> {
    let condition = *condition;
    match condition.kind {
        ExprKind::Binary { op, left, right }
            if matches!(
                op,
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
            ) =>
        {
            Ok((op, *left, *right))
        }
        kind => Err(Box::new(Expr { kind, ..condition })),
    }
}

fn channel_is_test_failure(resolver: &Resolver<'_>) -> bool {
    let Some(channel) = &resolver.error_channel else {
        return false;
    };
    let Some((failure_id, _)) = resolver.registry.lookup(&Identifier::new(
        TEST_PACKAGE,
        vec![FAILURE_TYPE.to_string()],
    )) else {
        return false;
    };
    matches!(
        peel_alias(&channel.error, resolver.registry),
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } if id == failure_id
    )
}

fn channel_diagnostic(resolver: &Resolver<'_>, span: Span) -> Diagnostic {
    let test_linked = resolver
        .registry
        .lookup(&Identifier::new(
            TEST_PACKAGE,
            vec![FAILURE_TYPE.to_string()],
        ))
        .is_some();
    if test_linked {
        Diagnostic::error_with_hint(
            "`assert` needs `Test.Failure` on the error channel",
            "use it inside a `test` body or a helper that declares `! Test.Failure`",
            span,
        )
    } else {
        Diagnostic::error_with_hint(
            "`assert` needs the `Test` package, which only `koja test` and `koja check` link",
            "move this into a test file, or use `Kernel.panic` for an invariant in \
             application code",
            span,
        )
    }
}

fn assertion_construction(
    source: &AssertSource,
    condition_span: Span,
    left: Expr,
    right: Expr,
    message: Expr,
    span: Span,
) -> Expr {
    let field = |name: &str, value: Expr| FieldInit {
        name: Name::new(name, span),
        value,
        span,
    };
    Expr::new(
        ExprKind::StructConstruction {
            type_path: vec![
                Name::new(TEST_PACKAGE, span),
                Name::new(ASSERTION_TYPE, span),
            ],
            fields: vec![
                field("column", int_literal(condition_span.start.column, span)),
                field("expression", string_literal(&source.expression, span)),
                field("file", string_literal(&source.file, span)),
                field("left", left),
                field("line", int_literal(condition_span.start.line, span)),
                field("message", message),
                field("right", right),
                field("source_line", string_literal(&source.source_line, span)),
            ],
        },
        span,
    )
}

fn format_call(name: &str, span: Span) -> Expr {
    method_call(ident(name, span), "format", Vec::new(), span)
}

fn some(value: Expr, span: Span) -> Expr {
    option_construction("Some", EnumConstructionData::Tuple(vec![value]), span)
}

fn none(span: Span) -> Expr {
    option_construction("None", EnumConstructionData::Unit, span)
}

fn option_construction(variant: &str, data: EnumConstructionData, span: Span) -> Expr {
    Expr::new(
        ExprKind::EnumConstruction {
            type_path: vec![Name::new("Option", span)],
            variant: Name::new(variant, span),
            data,
        },
        span,
    )
}

fn string_literal(value: &str, span: Span) -> Expr {
    Expr::new(
        ExprKind::String {
            parts: vec![StringPart::Literal {
                value: value.to_string(),
                span,
            }],
            multiline: false,
        },
        span,
    )
}

fn int_literal(value: u32, span: Span) -> Expr {
    Expr::new(
        ExprKind::Literal {
            value: Literal::Int(value.to_string()),
        },
        span,
    )
}
