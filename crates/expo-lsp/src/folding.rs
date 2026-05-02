//! Folding range provider for the Expo LSP.
//!
//! Provides collapsible regions for `fn...end`, `struct...end`, `enum...end`,
//! `impl...end`, `protocol...end`, `if...end`, `match...end`, `for...end`,
//! `while...end`, `loop...end`, `cond...end`, `receive...end`, and contiguous
//! comment blocks.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use expo_ast::ast::{Comment, Expr, ExprKind, ImplMember, Item, Module, Statement};
use expo_ast::span::Span;

use crate::backend::Backend;

impl Backend {
    pub(crate) async fn handle_folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> Result<Option<Vec<FoldingRange>>> {
        let uri = params.text_document.uri;

        let docs = self.documents.read().await;
        let state = match docs.get(uri.as_str()) {
            Some(s) => s,
            None => return Ok(None),
        };

        let mut ranges = Vec::new();
        collect_item_folds(&state.file, &mut ranges);
        collect_comment_folds(&state.file.comments, &mut ranges);
        Ok(Some(ranges))
    }
}

fn span_fold(span: &Span, kind: Option<FoldingRangeKind>) -> Option<FoldingRange> {
    let start = span.start.line.saturating_sub(1);
    let end = span.end.line.saturating_sub(1);
    if start >= end {
        return None;
    }
    Some(FoldingRange {
        start_line: start,
        start_character: None,
        end_line: end,
        end_character: None,
        kind,
        collapsed_text: None,
    })
}

fn collect_item_folds(file: &Module, ranges: &mut Vec<FoldingRange>) {
    for item in &file.items {
        match item {
            Item::Alias(_) => {}
            Item::Function(f) => {
                if let Some(r) = span_fold(&f.span, Some(FoldingRangeKind::Region)) {
                    ranges.push(r);
                }
                if let Some(body) = &f.body {
                    collect_statement_folds(body, ranges);
                }
            }
            Item::Struct(s) => {
                if let Some(r) = span_fold(&s.span, Some(FoldingRangeKind::Region)) {
                    ranges.push(r);
                }
            }
            Item::Enum(e) => {
                if let Some(r) = span_fold(&e.span, Some(FoldingRangeKind::Region)) {
                    ranges.push(r);
                }
            }
            Item::Impl(imp) => {
                if let Some(r) = span_fold(&imp.span, Some(FoldingRangeKind::Region)) {
                    ranges.push(r);
                }
                for member in &imp.members {
                    if let ImplMember::Function(f) = member {
                        if let Some(r) = span_fold(&f.span, Some(FoldingRangeKind::Region)) {
                            ranges.push(r);
                        }
                        if let Some(body) = &f.body {
                            collect_statement_folds(body, ranges);
                        }
                    }
                }
            }
            Item::Protocol(p) => {
                if let Some(r) = span_fold(&p.span, Some(FoldingRangeKind::Region)) {
                    ranges.push(r);
                }
                for m in &p.methods {
                    if let Some(r) = span_fold(&m.span, Some(FoldingRangeKind::Region)) {
                        ranges.push(r);
                    }
                    if let Some(body) = &m.body {
                        collect_statement_folds(body, ranges);
                    }
                }
            }
            Item::Constant(c) => {
                if let Some(r) = span_fold(&c.span, Some(FoldingRangeKind::Region)) {
                    ranges.push(r);
                }
            }
            Item::TypeAlias(_) | Item::Shared(_) => {}
        }
    }
}

fn collect_statement_folds(stmts: &[Statement], ranges: &mut Vec<FoldingRange>) {
    for stmt in stmts {
        if let Statement::Expr(expr) = stmt {
            collect_expr_folds(expr, ranges);
        }
    }
}

fn collect_expr_folds(expr: &Expr, ranges: &mut Vec<FoldingRange>) {
    match &expr.kind {
        ExprKind::If {
            then_body,
            else_body,
            ..
        } => {
            if let Some(r) = span_fold(&expr.span, Some(FoldingRangeKind::Region)) {
                ranges.push(r);
            }
            collect_statement_folds(then_body, ranges);
            if let Some(eb) = else_body {
                collect_statement_folds(eb, ranges);
            }
        }
        ExprKind::Match { arms, .. } => {
            if let Some(r) = span_fold(&expr.span, Some(FoldingRangeKind::Region)) {
                ranges.push(r);
            }
            for arm in arms {
                collect_statement_folds(&arm.body, ranges);
            }
        }
        ExprKind::Cond {
            arms, else_body, ..
        } => {
            if let Some(r) = span_fold(&expr.span, Some(FoldingRangeKind::Region)) {
                ranges.push(r);
            }
            for arm in arms {
                collect_statement_folds(&arm.body, ranges);
            }
            if let Some(eb) = else_body {
                collect_statement_folds(eb, ranges);
            }
        }
        ExprKind::For { body, .. } => {
            if let Some(r) = span_fold(&expr.span, Some(FoldingRangeKind::Region)) {
                ranges.push(r);
            }
            collect_statement_folds(body, ranges);
        }
        ExprKind::While { body, .. } => {
            if let Some(r) = span_fold(&expr.span, Some(FoldingRangeKind::Region)) {
                ranges.push(r);
            }
            collect_statement_folds(body, ranges);
        }
        ExprKind::Loop { body, .. } => {
            if let Some(r) = span_fold(&expr.span, Some(FoldingRangeKind::Region)) {
                ranges.push(r);
            }
            collect_statement_folds(body, ranges);
        }
        ExprKind::Unless { body, .. } => {
            if let Some(r) = span_fold(&expr.span, Some(FoldingRangeKind::Region)) {
                ranges.push(r);
            }
            collect_statement_folds(body, ranges);
        }
        ExprKind::Receive {
            arms, after_body, ..
        } => {
            if let Some(r) = span_fold(&expr.span, Some(FoldingRangeKind::Region)) {
                ranges.push(r);
            }
            for arm in arms {
                collect_statement_folds(&arm.body, ranges);
            }
            collect_statement_folds(after_body, ranges);
        }
        ExprKind::Closure { body, .. } => {
            if let Some(r) = span_fold(&expr.span, Some(FoldingRangeKind::Region)) {
                ranges.push(r);
            }
            collect_statement_folds(body, ranges);
        }
        ExprKind::Arena { body, .. } => {
            if let Some(r) = span_fold(&expr.span, Some(FoldingRangeKind::Region)) {
                ranges.push(r);
            }
            collect_statement_folds(body, ranges);
        }
        _ => {}
    }
}

fn collect_comment_folds(comments: &[Comment], ranges: &mut Vec<FoldingRange>) {
    if comments.is_empty() {
        return;
    }

    let mut group_start = comments[0].span.start.line;
    let mut group_end = comments[0].span.end.line;

    for comment in &comments[1..] {
        let line = comment.span.start.line;
        if line == group_end + 1 {
            group_end = comment.span.end.line;
        } else {
            if group_start < group_end {
                ranges.push(FoldingRange {
                    start_line: group_start.saturating_sub(1),
                    start_character: None,
                    end_line: group_end.saturating_sub(1),
                    end_character: None,
                    kind: Some(FoldingRangeKind::Comment),
                    collapsed_text: None,
                });
            }
            group_start = line;
            group_end = comment.span.end.line;
        }
    }

    if group_start < group_end {
        ranges.push(FoldingRange {
            start_line: group_start.saturating_sub(1),
            start_character: None,
            end_line: group_end.saturating_sub(1),
            end_character: None,
            kind: Some(FoldingRangeKind::Comment),
            collapsed_text: None,
        });
    }
}
