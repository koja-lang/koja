//! Folding range provider for the Koja LSP.
//!
//! Provides collapsible regions for `fn...end`, `struct...end`, `enum...end`,
//! `impl...end`, `protocol...end`, `if...end`, `match...end`, `for...end`,
//! `while...end`, `loop...end`, `cond...end`, `receive...end`, and contiguous
//! comment blocks.

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;

use koja_ast::ast::{Comment, Expr, ExprKind, File, Function, Item, ProtocolMethod, TestDecl};
use koja_ast::span::Span;
use koja_ast::visit::{self, Visitor};

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
        if let Some(file) = state.active_file() {
            ranges = region_folds(file);
            collect_comment_folds(&file.comments, &mut ranges);
        }
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

/// One region fold per multi-line declaration and block expression.
fn region_folds(file: &File) -> Vec<FoldingRange> {
    let mut regions = Regions { ranges: Vec::new() };
    regions.visit_file(file);
    regions.ranges
}

struct Regions {
    ranges: Vec<FoldingRange>,
}

impl Regions {
    fn fold(&mut self, span: &Span) {
        if let Some(range) = span_fold(span, Some(FoldingRangeKind::Region)) {
            self.ranges.push(range);
        }
    }
}

impl<'ast> Visitor<'ast> for Regions {
    /// Functions and tests fold through their own hooks. A derived
    /// `impl` has no source of its own to fold.
    fn visit_item(&mut self, item: &'ast Item) {
        match item {
            Item::Alias(_) | Item::Function(_) | Item::Test(_) | Item::TypeAlias(_) => {}
            Item::Builtin(decl) => self.fold(&decl.span),
            Item::Constant(constant) => self.fold(&constant.span),
            Item::Enum(decl) => self.fold(&decl.span),
            Item::Extend(block) => self.fold(&block.span),
            Item::Impl(block) if block.span.synthetic => return,
            Item::Impl(block) => self.fold(&block.span),
            Item::Protocol(decl) => self.fold(&decl.span),
            Item::Struct(decl) => self.fold(&decl.span),
        }
        visit::walk_item(self, item);
    }

    fn visit_function(&mut self, function: &'ast Function) {
        self.fold(&function.span);
        visit::walk_function(self, function);
    }

    fn visit_protocol_method(&mut self, method: &'ast ProtocolMethod) {
        self.fold(&method.span);
        visit::walk_protocol_method(self, method);
    }

    fn visit_test(&mut self, test: &'ast TestDecl) {
        self.fold(&test.span);
        visit::walk_test(self, test);
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        if matches!(
            &expr.kind,
            ExprKind::Closure { .. }
                | ExprKind::Cond { .. }
                | ExprKind::For { .. }
                | ExprKind::If { .. }
                | ExprKind::Loop { .. }
                | ExprKind::Match { .. }
                | ExprKind::Receive { .. }
                | ExprKind::While { .. }
        ) {
            self.fold(&expr.span);
        }
        visit::walk_expr(self, expr);
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
