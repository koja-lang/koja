//! The regions of a file an editor can fold.
//!
//! A block region is a declaration or a block expression that spans
//! more than one line. A comment region is a run of comments on
//! consecutive lines. Lines count from 1, as spans do.

use koja_ast::ast::{Comment, Expr, ExprKind, File, Function, Item, ProtocolMethod, TestDecl};
use koja_ast::span::Span;
use koja_ast::visit::{self, Visitor};

/// A foldable range of lines. `end_line` is always past
/// `start_line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub end_line: u32,
    pub kind: RegionKind,
    pub start_line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    Block,
    Comment,
}

/// Every foldable region in `file`. Blocks come first in walk
/// order, then comment runs. A derived `impl` has no source of its
/// own and is left out.
pub fn regions(file: &File) -> Vec<Region> {
    let mut blocks = Blocks {
        regions: Vec::new(),
    };
    blocks.visit_file(file);
    let mut regions = blocks.regions;
    comment_runs(&file.comments, &mut regions);
    regions
}

/// The region a span covers, or `None` when it fits on one line.
fn region(span: &Span, kind: RegionKind) -> Option<Region> {
    let (start_line, end_line) = (span.start.line, span.end.line);
    (start_line < end_line).then_some(Region {
        end_line,
        kind,
        start_line,
    })
}

struct Blocks {
    regions: Vec<Region>,
}

impl Blocks {
    fn fold(&mut self, span: &Span) {
        self.regions.extend(region(span, RegionKind::Block));
    }
}

impl<'ast> Visitor<'ast> for Blocks {
    /// Functions and tests fold through their own hooks.
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

/// One comment region per run of comments on consecutive lines. A
/// comment on its own does not fold.
fn comment_runs(comments: &[Comment], regions: &mut Vec<Region>) {
    let Some(first) = comments.first() else {
        return;
    };
    let mut run = (first.span.start.line, first.span.end.line);
    for comment in &comments[1..] {
        if comment.span.start.line == run.1 + 1 {
            run.1 = comment.span.end.line;
            continue;
        }
        regions.extend(comment_region(run));
        run = (comment.span.start.line, comment.span.end.line);
    }
    regions.extend(comment_region(run));
}

fn comment_region((start_line, end_line): (u32, u32)) -> Option<Region> {
    (start_line < end_line).then_some(Region {
        end_line,
        kind: RegionKind::Comment,
        start_line,
    })
}

#[cfg(test)]
mod tests {
    use koja_parser::{ParseMode, parse};

    use super::*;

    fn regions_of(source: &str) -> Vec<Region> {
        let parsed = parse(source, ParseMode::File);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        regions(&parsed.ast)
    }

    fn block(start_line: u32, end_line: u32) -> Region {
        Region {
            end_line,
            kind: RegionKind::Block,
            start_line,
        }
    }

    fn comment(start_line: u32, end_line: u32) -> Region {
        Region {
            end_line,
            kind: RegionKind::Comment,
            start_line,
        }
    }

    #[test]
    fn declarations_and_block_expressions_fold() {
        let regions = regions_of(
            "struct Point\n  x: Int\nend\n\
             fn run(flag: Bool) -> Int\n  if flag\n    1\n  else\n    2\n  end\nend\n",
        );
        assert_eq!(regions, vec![block(1, 3), block(4, 10), block(5, 9)]);
    }

    #[test]
    fn one_line_declarations_do_not_fold() {
        let regions = regions_of("fn one() -> Int 1 end\nconst K = 1\n");
        assert!(regions.is_empty(), "{regions:?}");
    }

    #[test]
    fn consecutive_comments_fold_as_one_run() {
        let regions = regions_of(
            "# one\n# two\n# three\nfn run() -> Int\n  1\nend\n# alone\n\n# four\n# five\n",
        );
        assert_eq!(regions, vec![block(4, 6), comment(1, 3), comment(9, 10)]);
    }
}
