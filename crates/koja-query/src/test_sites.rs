//! Where the tests in a file are, for a run-test lens.
//!
//! Typecheck desugars every `test "..."` block into a function with
//! [`FunctionOrigin::Test`] that keeps the block's span, so the
//! post-check AST has no [`TestDecl`] left to find. The description
//! goes with it. A lens does not need it. The spec id `koja test
//! --only` takes is `file:line`, and the line is the span's first
//! line, the same one test discovery records.
//!
//! [`TestDecl`]: koja_ast::ast::TestDecl

use koja_ast::ast::{Function, FunctionOrigin};
use koja_ast::span::{FileId, Span};
use koja_ast::visit::{self, Visitor};

use crate::Analysis;

/// The span of every test in `file`, in source order. A `test` block
/// spans from its keyword to its `end`.
pub fn tests_in_file(analysis: &Analysis<'_>, file: FileId) -> Vec<Span> {
    let Some(ast) = analysis.file(file) else {
        return Vec::new();
    };
    let mut collector = Collector { spans: Vec::new() };
    collector.visit_file(ast);
    collector
        .spans
        .sort_by_key(|span| (span.start.line, span.start.column));
    collector.spans
}

struct Collector {
    spans: Vec<Span>,
}

impl<'ast> Visitor<'ast> for Collector {
    fn visit_function(&mut self, function: &'ast Function) {
        if function.origin == FunctionOrigin::Test {
            self.spans.push(function.span);
        }
        visit::walk_function(self, function);
    }
}
