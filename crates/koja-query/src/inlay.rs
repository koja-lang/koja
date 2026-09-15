//! Inlay hints, the editor's inline annotations for inferred binding
//! types and parameter names at call sites.
//!
//! Every hint is a stamp read. Binding types come from the value's
//! `Expr.resolution`. Parameter names come from the call's resolved
//! target and its lifted signature. Nothing here infers a type or
//! resolves a name.

use koja_ast::ast::*;
use koja_ast::identifier::{AnonymousKind, Resolution, ResolvedType};
use koja_ast::span::{FileId, Position, Span};
use koja_typecheck::{FunctionSignature, GlobalRegistry};

use crate::Analysis;
use crate::display::format_resolved_type;
use crate::expr_at::signature_for_target;
use crate::index::{ReferenceIndex, Role};
use crate::visit::{self, Visitor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    /// `: T` after a binding whose type the author left out.
    Type,
    /// `name:` before a positional argument.
    Parameter,
}

/// One inline annotation. `position` is 1-indexed, the same as every
/// [`Span`] in the AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub position: Position,
    pub label: String,
    pub kind: HintKind,
}

/// Every hint in `file` whose position falls inside `range`, in
/// source order.
pub fn hints(
    analysis: &Analysis<'_>,
    index: &ReferenceIndex,
    file: FileId,
    range: Span,
) -> Vec<Hint> {
    let Some(ast) = analysis.file(file) else {
        return Vec::new();
    };
    let mut collector = Collector {
        registry: analysis.registry,
        index,
        file,
        range,
        hints: Vec::new(),
    };
    collector.visit_file(ast);
    collector
        .hints
        .sort_by_key(|hint| (hint.position.line, hint.position.column));
    collector.hints
}

struct Collector<'a> {
    registry: &'a GlobalRegistry,
    index: &'a ReferenceIndex,
    file: FileId,
    range: Span,
    hints: Vec<Hint>,
}

impl Collector<'_> {
    fn in_range(&self, position: Position) -> bool {
        let Span { start, end, .. } = self.range;
        (start.line, start.column) <= (position.line, position.column)
            && (position.line, position.column) <= (end.line, end.column)
    }

    fn push(&mut self, position: Position, label: String, kind: HintKind) {
        if self.in_range(position) {
            self.hints.push(Hint {
                position,
                label,
                kind,
            });
        }
    }

    fn type_hint(&mut self, name: &Name, ty: &ResolvedType) {
        if name.span.synthetic || !ty.is_resolved() {
            return;
        }
        let label = format!(": {}", format_resolved_type(ty, self.registry));
        self.push(name.span.end, label, HintKind::Type);
    }

    /// `x = expr` declares `x` the first time and rebinds it after.
    /// Only the declaration gets a hint, and only when the author
    /// left the annotation out.
    fn assignment(&mut self, target: &LValue, type_annotation: Option<&TypeExpr>, value: &Expr) {
        if type_annotation.is_some() {
            return;
        }
        let [head] = target.segments.as_slice() else {
            return;
        };
        let declares = self
            .index
            .occurrence_at(self.file, head.span.start.line, head.span.start.column)
            .is_some_and(|occurrence| {
                occurrence.role == Role::Declaration && occurrence.span == head.span
            });
        if declares {
            self.type_hint(head, &value.resolution);
        }
    }

    /// `(a, b) = expr` over a flat tuple. Each binding takes the
    /// matching element of the value's tuple type.
    fn destructure(&mut self, pattern: &Pattern, value: &Expr) {
        let Pattern::Tuple { elements, .. } = pattern else {
            return;
        };
        let ResolvedType::Anonymous(AnonymousKind::Tuple { elements: types }) = &value.resolution
        else {
            return;
        };
        for (element, ty) in elements.iter().zip(types) {
            if let Pattern::Binding { name, .. } = element {
                self.type_hint(name, ty);
            }
        }
    }

    /// Untyped closure parameters take their type from the closure's
    /// own function type.
    fn closure_params(&mut self, params: &[ClosureParam], closure: &Expr) {
        let ResolvedType::Anonymous(AnonymousKind::Function { params: types, .. }) =
            &closure.resolution
        else {
            return;
        };
        for (param, ty) in params.iter().zip(types) {
            if let ClosureParam::Name {
                name,
                type_expr: None,
                ..
            } = param
            {
                self.type_hint(name, ty);
            }
        }
    }

    /// `name:` before each positional argument. A trailing `self` in
    /// the signature is the receiver of an instance call and has no
    /// argument to label.
    fn parameters(&mut self, signature: &FunctionSignature, args: &[Arg]) {
        let mut params = signature.params.as_slice();
        if params.len() == args.len() + 1 && params[0].name == "self" {
            params = &params[1..];
        }
        for (param, arg) in params.iter().zip(args) {
            if arg.name.is_some() || arg.span.synthetic || param.name.starts_with('_') {
                continue;
            }
            if let ExprKind::Ident { name, .. } = &arg.value.kind
                && *name == param.name
            {
                continue;
            }
            self.push(
                arg.span.start,
                format!("{}:", param.name),
                HintKind::Parameter,
            );
        }
    }

    fn call(&mut self, target: Resolution, args: &[Arg]) {
        if let Some(signature) = signature_for_target(target, self.registry) {
            self.parameters(signature, args);
        }
    }
}

impl<'ast> Visitor<'ast> for Collector<'_> {
    fn visit_statement(&mut self, statement: &'ast Statement) {
        match statement {
            Statement::Assignment {
                target,
                type_annotation,
                value,
                ..
            } => self.assignment(target, type_annotation.as_ref(), value),
            Statement::Destructure { pattern, value, .. } => self.destructure(pattern, value),
            _ => {}
        }
        visit::walk_statement(self, statement);
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        if expr.span.synthetic {
            return;
        }
        match &expr.kind {
            ExprKind::Call { callee, args, .. } => {
                if let ExprKind::Ident { resolution, .. } = &callee.kind {
                    self.call(*resolution, args);
                }
            }
            ExprKind::MethodCall {
                method,
                args,
                target,
                ..
            } if !method.span.synthetic => self.call(*target, args),
            ExprKind::Closure { params, .. } | ExprKind::ShortClosure { params, .. } => {
                self.closure_params(params, expr);
            }
            _ => {}
        }
        visit::walk_expr(self, expr);
    }
}
