//! The statement, expression, arm, pattern, and chain walks.

use koja_ast::ast::*;
use koja_ast::labels::pattern_span;
use koja_ast::span::Span;
use koja_ast::token::TokenKind;

use super::super::util::{chain_links, map_entry_span, stmt_span};
use super::{Attacher, ChildInfo, Slot, content_end_line, first_offset, first_stmt_offset};

impl Attacher<'_> {
    pub(super) fn walk_stmt(&mut self, stmt: &Statement) {
        if !self.pending_before(stmt_span(stmt).end.offset) {
            return;
        }
        match stmt {
            Statement::Expr(expr) => self.walk_expr(expr),
            Statement::Assignment { value, .. } | Statement::CompoundAssign { value, .. } => {
                // A comment after `=` on a broken assignment's head line
                // has no slot of its own, so hoist it above the statement.
                let head = self.take_on_line(stmt_span(stmt).start.line, value.span.start.offset);
                self.push(stmt_span(stmt), Slot::Leading, head);
                self.walk_expr(value);
            }
            Statement::Destructure { pattern, value, .. } => {
                self.walk_pattern(pattern);
                let head = self.take_on_line(stmt_span(stmt).start.line, value.span.start.offset);
                self.push(stmt_span(stmt), Slot::Leading, head);
                self.walk_expr(value);
            }
            Statement::Return { value, .. } => {
                if let Some(v) = value {
                    self.walk_expr(v);
                }
            }
            Statement::Break { .. } => {}
        }
    }

    pub(super) fn walk_expr(&mut self, expr: &Expr) {
        if !self.pending_before(expr.span.end.offset) {
            return;
        }
        match &expr.kind {
            ExprKind::Ident { .. }
            | ExprKind::Literal { .. }
            | ExprKind::NamedFunctionReference { .. }
            | ExprKind::Self_ { .. } => {}

            ExprKind::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::Group { expr: inner }
            | ExprKind::Spawn { expr: inner }
            | ExprKind::Try { expr: inner } => self.walk_expr(inner),
            ExprKind::Fail { value } => self.walk_expr(value),
            ExprKind::Assert {
                condition, message, ..
            } => {
                self.walk_expr(condition);
                if let Some(message) = message {
                    self.walk_expr(message);
                }
            }
            ExprKind::FieldAccess { receiver, .. } => self.walk_expr(receiver),
            ExprKind::Rescue {
                subject, handler, ..
            } => {
                self.walk_expr(subject);
                self.walk_expr(handler);
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.walk_expr(condition);
                self.walk_expr(then_expr);
                self.walk_expr(else_expr);
            }
            ExprKind::ShortClosure { body, .. } => self.walk_expr(body),
            ExprKind::String { parts, .. } => {
                for part in parts {
                    if let StringPart::Interpolation { expr: inner, .. } = part {
                        self.walk_expr(inner);
                    }
                }
            }

            ExprKind::Call { callee, args, .. } => {
                self.walk_expr(callee);
                self.walk_args(args, expr.span);
            }
            ExprKind::MethodCall { .. } => self.walk_chain(expr),

            ExprKind::List { elements } | ExprKind::Tuple { elements } => {
                self.walk_children(
                    elements,
                    |e| ChildInfo::of(e.span),
                    |a, e| a.walk_expr(e),
                    expr.span.end.offset,
                    (expr.span, Slot::Stragglers),
                );
            }
            ExprKind::Map { entries } => {
                self.walk_children(
                    entries,
                    |(k, v)| ChildInfo::of(map_entry_span(k, v)),
                    |a, (k, v)| {
                        a.walk_expr(k);
                        a.walk_expr(v);
                    },
                    expr.span.end.offset,
                    (expr.span, Slot::Stragglers),
                );
            }
            ExprKind::BinaryLiteral { segments } => {
                self.walk_children(
                    segments,
                    |s| ChildInfo::of(s.span),
                    |a, s| a.walk_expr(&s.value),
                    expr.span.end.offset,
                    (expr.span, Slot::Stragglers),
                );
            }
            ExprKind::StructConstruction { fields, .. } => self.walk_field_inits(fields, expr.span),
            ExprKind::EnumConstruction { data, .. } => match data {
                EnumConstructionData::Unit => {}
                EnumConstructionData::Tuple(exprs) => {
                    for e in exprs {
                        self.walk_expr(e);
                    }
                }
                EnumConstructionData::Struct(fields) => self.walk_field_inits(fields, expr.span),
            },

            ExprKind::Closure { body, .. } => {
                self.walk_block(expr.span, expr.span.start.line, body)
            }
            ExprKind::Loop { body } => self.walk_block(expr.span, expr.span.start.line, body),
            ExprKind::While { condition, body } => {
                self.walk_expr(condition);
                self.walk_block(expr.span, condition.span.end.line, body);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
            } => {
                self.walk_pattern(pattern);
                self.walk_expr(iterable);
                self.walk_block(expr.span, iterable.span.end.line, body);
            }
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => self.walk_if(expr.span, condition, then_body, else_body.as_deref()),
            ExprKind::Match { subject, arms } => self.walk_match(expr.span, subject, arms),
            ExprKind::Cond { arms, else_body } => {
                self.walk_cond(expr.span, arms, else_body.as_deref());
            }
            ExprKind::Receive {
                arms,
                after_timeout,
                after_body,
            } => self.walk_receive(expr.span, arms, after_timeout.as_deref(), after_body),
        }
    }

    fn walk_if(
        &mut self,
        owner: Span,
        condition: &Expr,
        then_body: &[Statement],
        else_body: Option<&[Statement]>,
    ) {
        self.walk_expr(condition);
        let Some(else_stmts) = else_body else {
            self.walk_block(owner, condition.span.end.line, then_body);
            return;
        };
        let first_then = first_stmt_offset(then_body, owner.end.offset);
        self.take_header_trailing(owner, condition.span.end.line, first_then);
        let search_from = then_body
            .last()
            .map_or(condition.span.end.offset, |s| stmt_span(s).end.offset);
        let else_offset = self.boundary_offset(TokenKind::Else, search_from, owner);
        self.walk_body(then_body, else_offset, owner);
        // The body walk routed the boundary comments to Dangling. With an
        // `else` they sit above it.
        let before_else = self.table.take(owner, Slot::Dangling);
        self.push(owner, Slot::BeforeElse, before_else);
        self.walk_else(owner, else_offset, else_stmts);
    }

    fn walk_match(&mut self, owner: Span, subject: &Expr, arms: &[MatchArm]) {
        self.walk_expr(subject);
        let first_arm = first_offset(arms, |a| a.span, owner.end.offset);
        self.take_header_trailing(owner, subject.span.end.line, first_arm);
        self.walk_match_arms(arms, owner.end.offset, owner, true);
    }

    fn walk_cond(&mut self, owner: Span, arms: &[CondArm], else_body: Option<&[Statement]>) {
        let first_arm = first_offset(arms, |a| a.span, owner.end.offset);
        self.take_header_trailing(owner, owner.start.line, first_arm);

        let arms_end = match else_body {
            Some(_) => {
                let search_from = arms
                    .last()
                    .map_or(owner.start.offset, |a| a.span.end.offset);
                self.boundary_offset(TokenKind::Else, search_from, owner)
            }
            None => owner.end.offset,
        };
        for (i, arm) in arms.iter().enumerate() {
            let is_last = i + 1 == arms.len() && else_body.is_none();
            let next_offset = arms.get(i + 1).map_or(arms_end, |a| a.span.start.offset);
            self.walk_cond_arm(arm, is_last, next_offset);
        }
        match else_body {
            Some(else_stmts) => {
                let before_else = self.take_before(arms_end);
                self.push(owner, Slot::BeforeElse, before_else);
                self.walk_else(owner, arms_end, else_stmts);
            }
            None => {
                let rest = self.take_before(owner.end.offset);
                self.push(owner, Slot::Dangling, rest);
            }
        }
    }

    fn walk_receive(
        &mut self,
        owner: Span,
        arms: &[MatchArm],
        after_timeout: Option<&Expr>,
        after_body: &[Statement],
    ) {
        let first_arm = first_offset(arms, |a| a.span, owner.end.offset);
        self.take_header_trailing(owner, owner.start.line, first_arm);
        let Some(timeout) = after_timeout else {
            self.walk_match_arms(arms, owner.end.offset, owner, true);
            return;
        };
        let search_from = arms
            .last()
            .map_or(owner.start.offset, |a| a.span.end.offset);
        let arms_end = self.boundary_offset(TokenKind::After, search_from, owner);
        self.walk_match_arms(arms, arms_end, owner, false);
        // The arms walk routed the boundary comments to Dangling. With an
        // `after` clause they sit above it.
        let before_after = self.table.take(owner, Slot::Dangling);
        self.push(owner, Slot::BeforeAfter, before_after);
        self.walk_expr(timeout);
        let first_stmt = first_stmt_offset(after_body, owner.end.offset);
        self.take_header_trailing(timeout.span, timeout.span.end.line, first_stmt);
        self.walk_body(after_body, owner.end.offset, owner);
    }

    /// The `else` keyword's trailing comment, then the else body.
    /// `search_from` is at or before the keyword.
    fn walk_else(&mut self, owner: Span, search_from: u32, else_stmts: &[Statement]) {
        if let Some(token) = self.find_token(TokenKind::Else, search_from, owner.end.offset) {
            let first_else = first_stmt_offset(else_stmts, owner.end.offset);
            let trailing = self.take_on_line(token.span.start.line, first_else);
            self.push(owner, Slot::ElseTrailing, trailing);
        }
        self.walk_body(else_stmts, owner.end.offset, owner);
    }

    /// Offset of the `else` or `after` keyword that ends a region,
    /// searched from `from`. The owner's end when the keyword is missing.
    fn boundary_offset(&self, kind: TokenKind, from: u32, owner: Span) -> u32 {
        self.find_token(kind, from, owner.end.offset)
            .map_or(owner.end.offset, |t| t.span.start.offset)
    }

    /// Walks match/receive arms. `bound` starts the region after the arms.
    /// `region_ends` keeps comments before `end` on the owner.
    fn walk_match_arms(&mut self, arms: &[MatchArm], bound: u32, owner: Span, region_ends: bool) {
        for (i, arm) in arms.iter().enumerate() {
            let is_last = i + 1 == arms.len() && region_ends;
            let next_offset = arms.get(i + 1).map_or(bound, |a| a.span.start.offset);
            let leading = self.take_before(arm.span.start.offset);
            self.push(arm.span, Slot::Leading, leading);
            self.walk_pattern(&arm.pattern);

            let head_end = arm
                .guard
                .as_ref()
                .map_or(pattern_span(&arm.pattern).end.line, |g| g.span.end.line);
            if let Some(guard) = &arm.guard {
                self.walk_expr(guard);
            }
            self.walk_arm_interior(arm.span, head_end, &arm.body, is_last, next_offset);
        }
        let rest = self.take_before(bound);
        self.push(owner, Slot::Dangling, rest);
    }

    fn walk_cond_arm(&mut self, arm: &CondArm, is_last: bool, next_offset: u32) {
        let leading = self.take_before(arm.span.start.offset);
        self.push(arm.span, Slot::Leading, leading);
        self.walk_expr(&arm.condition);
        self.walk_arm_interior(
            arm.span,
            arm.condition.span.end.line,
            &arm.body,
            is_last,
            next_offset,
        );
    }

    /// Shared arm interior: head-line trailing, body, and the arm's
    /// boundary policy. A comment run directly after a non-final arm
    /// stays with that arm. A blank line before the run makes it lead
    /// the next arm. Region-final comments belong to the enclosing
    /// construct and dangle before `end`, `else`, or `after`.
    fn walk_arm_interior(
        &mut self,
        arm_span: Span,
        head_end: u32,
        body: &[Statement],
        is_last: bool,
        next_offset: u32,
    ) {
        let first_stmt = first_stmt_offset(body, arm_span.end.offset);
        // A wrapped head has no stable per-line anchors. Comments before its
        // final line hoist above the arm, while only the final-line comment
        // trails the canonical head.
        let head_comments = self.take_before(first_stmt.min(arm_span.end.offset));
        let mut above_body = Vec::new();
        let mut above_head = Vec::new();
        let mut on_head = Vec::new();
        for comment in head_comments {
            if comment.span.start.line < head_end {
                above_head.push(comment);
            } else if comment.span.start.line == head_end {
                on_head.push(comment);
            } else {
                above_body.push(comment);
            }
        }
        self.push(arm_span, Slot::Leading, above_head);
        self.push(arm_span, Slot::HeaderTrailing, on_head);
        if let Some(first) = body.first() {
            self.push(stmt_span(first), Slot::Leading, above_body);
        } else {
            self.push(arm_span, Slot::Dangling, above_body);
        }

        self.walk_body(body, arm_span.end.offset, arm_span);
        if !is_last {
            let body_comments =
                self.take_before_without_blank(next_offset, content_end_line(arm_span));
            self.push(arm_span, Slot::Dangling, body_comments);
        }
        // An inline arm's trailing comment stays on its line.
        let trailing = self.take_on_line(content_end_line(arm_span), next_offset);
        self.push(arm_span, Slot::Trailing, trailing);
    }

    /// Walks a pattern so a comment inside a broken container anchors to
    /// its element instead of relocating to the enclosing head line.
    /// Comments between or-pattern alternatives stay unclaimed (the fill
    /// layout has no per-line anchor) and relocate via the caller's sweep.
    fn walk_pattern(&mut self, pattern: &Pattern) {
        let span = pattern_span(pattern);
        let inside = self.peek().is_some_and(|c| {
            span.start.offset < c.span.start.offset && c.span.start.offset < span.end.offset
        });
        if !inside {
            return;
        }
        match pattern {
            Pattern::Binary { segments, .. } if !segments.is_empty() => {
                self.walk_children(
                    segments,
                    |s| ChildInfo::of(s.span),
                    |_, _| {},
                    span.end.offset,
                    (span, Slot::Stragglers),
                );
            }
            Pattern::Constructor { elements, .. }
            | Pattern::EnumTuple { elements, .. }
            | Pattern::List { elements, .. }
            | Pattern::Tuple { elements, .. }
                if !elements.is_empty() =>
            {
                self.walk_children(
                    elements,
                    |p| ChildInfo::of(pattern_span(p)),
                    |a, p| a.walk_pattern(p),
                    span.end.offset,
                    (span, Slot::Stragglers),
                );
            }
            Pattern::EnumStruct { fields, .. } | Pattern::Struct { fields, .. } => {
                self.walk_children(
                    fields,
                    |f| ChildInfo::of(f.span),
                    |a, f| a.walk_pattern(&f.pattern),
                    span.end.offset,
                    (span, Slot::Stragglers),
                );
            }
            Pattern::Or { patterns, .. } => {
                for alternative in patterns {
                    self.walk_pattern(alternative);
                }
            }
            _ => {}
        }
    }

    fn walk_args(&mut self, args: &[Arg], call_span: Span) {
        self.walk_children(
            args,
            |a| ChildInfo::of(a.span),
            |s, a| s.walk_expr(&a.value),
            call_span.end.offset,
            (call_span, Slot::Stragglers),
        );
    }

    fn walk_field_inits(&mut self, fields: &[FieldInit], owner: Span) {
        self.walk_children(
            fields,
            |f| ChildInfo::of(f.span),
            |s, f| s.walk_expr(&f.value),
            owner.end.offset,
            (owner, Slot::Stragglers),
        );
    }

    /// Walks a method chain, root first, then each link outward. Link
    /// comments are keyed by the receiver's span because the outermost
    /// link's own span is the whole chain, which is also the
    /// sequence-child key for its leading comments, and the two must not
    /// collide.
    fn walk_chain(&mut self, expr: &Expr) {
        let (root, links) = chain_links(expr);
        self.walk_expr(root);

        for (i, link) in links.iter().enumerate() {
            let ExprKind::MethodCall { args, receiver, .. } = &link.kind else {
                unreachable!()
            };
            // Only real chains (2+ links) render link-leading comments.
            // A single call routes them into its argument list instead.
            if links.len() > 1 {
                let lead_offset = first_offset(args, |a| a.span, link.span.end.offset);
                let leading = self.take_before(lead_offset);
                self.push(receiver.span, Slot::Leading, leading);
            }
            self.walk_args(args, link.span);
            let next_offset = links
                .get(i + 1)
                .and_then(|next| {
                    let ExprKind::MethodCall { args, .. } = &next.kind else {
                        return None;
                    };
                    args.first().map(|a| a.span.start.offset)
                })
                .unwrap_or(expr.span.end.offset);
            if i + 1 < links.len() {
                let trailing = self.take_on_line(content_end_line(link.span), next_offset);
                self.push(receiver.span, Slot::Trailing, trailing);
            }
        }
    }
}
