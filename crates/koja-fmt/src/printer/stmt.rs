//! Statement and body printers.

use crate::doc::*;
use koja_ast::ast::*;
use koja_ast::span::Span;

use super::Printer;
use super::seq::{SeqEntry, Spacing, vertical};
use super::util::*;

impl Printer {
    /// Formats an indented body block (the statements between a keyword
    /// and `end`). `dangling` holds the comments between the last
    /// statement and the terminator.
    pub(super) fn body_to_doc(&mut self, stmts: &[Statement], dangling: Vec<Comment>) -> Doc {
        if stmts.is_empty() && dangling.is_empty() {
            return nil();
        }
        indent(
            2,
            concat(vec![hardline(), self.statements_to_doc(stmts, dangling)]),
        )
    }

    /// Renders a list of statements with their attached comments, ending
    /// with the region's `dangling` comments.
    pub(super) fn statements_to_doc(&mut self, stmts: &[Statement], dangling: Vec<Comment>) -> Doc {
        let entries: Vec<SeqEntry> = stmts
            .iter()
            .map(|stmt| {
                let key = stmt_span(stmt);
                let doc = self.statement_to_doc(stmt);
                self.entry(key, key.start.line, stmt_is_block(stmt), doc)
            })
            .collect();
        vertical(entries, Spacing::Preserve, dangling)
    }

    pub(super) fn statement_to_doc(&mut self, stmt: &Statement) -> Doc {
        match stmt {
            Statement::Assignment {
                target,
                type_annotation,
                value,
                span,
            } => {
                let target_doc = text(path_text(&target.segments));
                let lhs = match type_annotation {
                    Some(te) => concat(vec![target_doc, text(": "), type_expr_to_doc(te)]),
                    None => target_doc,
                };
                self.assignment_to_doc(lhs, value, *span)
            }
            Statement::Break { .. } => text("break"),
            Statement::CompoundAssign {
                target, op, value, ..
            } => {
                let lhs = text(path_text(&target.segments));
                self.assign_to_doc(lhs, compound_op_str(op), value)
            }
            Statement::Destructure { pattern, value, .. } => {
                let lhs = self.pattern_to_doc(pattern);
                self.assign_to_doc(lhs, "=", value)
            }
            Statement::Expr(expr) => self.expr_to_doc(expr),
            Statement::Return { value, .. } => match value {
                Some(v) => concat(vec![text("return "), self.expr_to_doc(v)]),
                None => text("return"),
            },
        }
    }

    /// A plain `lhs = value` assignment. Two value shapes get special
    /// treatment: an inline closure and a heredoc.
    fn assignment_to_doc(&mut self, lhs: Doc, value: &Expr, span: Span) -> Doc {
        if self.closure_renders_inline(value) {
            // Stay inline when the closure fits, breaking after `=`
            // (soft line) only when it overflows the line width.
            let value_doc = self.expr_to_doc(value);
            return group(concat(vec![
                lhs,
                text(" ="),
                indent(2, concat(vec![line(), value_doc])),
            ]));
        }
        if is_heredoc(value) {
            // Both opener placements are idiomatic. Preserve the
            // author's choice: glued (`x = """`) when the literal
            // starts on the assignment's line, otherwise broken
            // (newline after `=`, block indented like match/cond).
            let value_doc = self.expr_to_doc(value);
            let glued = value.span.start.line == span.start.line;
            return if glued {
                concat(vec![lhs, text(" = "), value_doc])
            } else {
                broken_assign_doc(lhs, "=", value_doc)
            };
        }
        self.assign_to_doc(lhs, "=", value)
    }

    /// `lhs op value`, with the value on its own indented line when it
    /// renders as a block.
    fn assign_to_doc(&mut self, lhs: Doc, op: &str, value: &Expr) -> Doc {
        // Decided before rendering: rendering consumes the comment
        // table the predicate consults.
        let breaks = self.forces_assignment_break(value);
        let value_doc = self.expr_to_doc(value);
        if breaks {
            broken_assign_doc(lhs, op, value_doc)
        } else {
            group(concat(vec![lhs, text(format!(" {op} ")), value_doc]))
        }
    }

    /// True when an assigned value renders as a multi-line block, which
    /// forces the break after `=`. A closure that renders inline does
    /// not count, so `ref = Task.async(fn () -> Int 42 end)` stays glued.
    fn forces_assignment_break(&self, expr: &Expr) -> bool {
        if matches!(expr.kind, ExprKind::Closure { .. }) {
            return !self.closure_renders_inline(expr);
        }
        if is_block_expr(expr) {
            return true;
        }
        match &expr.kind {
            ExprKind::Binary { right, .. } => self.forces_assignment_break(right),
            ExprKind::Call { args, .. } => {
                args.iter().any(|a| self.forces_assignment_break(&a.value))
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                self.forces_assignment_break(receiver)
                    || args.iter().any(|a| self.forces_assignment_break(&a.value))
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.forces_assignment_break(condition)
                    || self.forces_assignment_break(then_expr)
                    || self.forces_assignment_break(else_expr)
            }
            _ => false,
        }
    }
}

/// `lhs op` with the value on its own line, indented two. The shape
/// every assignment takes when its value is a block.
fn broken_assign_doc(lhs: Doc, op: &str, value_doc: Doc) -> Doc {
    concat(vec![
        lhs,
        text(format!(" {op}")),
        indent(2, concat(vec![hardline(), value_doc])),
    ])
}

fn compound_op_str(op: &CompoundOp) -> &'static str {
    match op {
        CompoundOp::Add => "+=",
        CompoundOp::Div => "/=",
        CompoundOp::Mul => "*=",
        CompoundOp::Sub => "-=",
    }
}
