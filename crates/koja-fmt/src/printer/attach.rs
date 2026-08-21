//! Eager comment attachment.
//!
//! Before printing, [`attach`] walks the AST in source-offset order with a
//! forward cursor over the comment list and assigns every comment to an
//! owner slot in a [`CommentTable`]. The printer then consumes slots by
//! `(Span, Slot)` key, in any order. Ownership is decided here, once. A
//! comment on a construct's last line trails that construct, and any
//! other comment belongs to the innermost enclosing block, leading the
//! next sibling inside it or dangling before the block's terminator.
//!
//! The `else` and `after` keywords have no spans in the AST, so the pass
//! locates them in a token stream lexed from the same source.

use std::collections::HashMap;

use koja_ast::ast::*;
use koja_ast::labels::pattern_span;
use koja_ast::span::Span;
use koja_ast::token::{Token, TokenKind};

use super::util::{
    item_annotations, item_span, map_entry_span, param_span, signature_end_line, stmt_span,
};

/// Where a comment sits relative to its owner.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Slot {
    /// Above the `after` keyword of a `receive`.
    BeforeAfter,
    /// Above the `else` keyword of an `if` or `cond`.
    BeforeElse,
    /// Between the last child and the block's `end`.
    Dangling,
    /// On the `else` keyword's line, after it.
    ElseTrailing,
    /// On a block header or arm-head line, after the code.
    HeaderTrailing,
    /// Above the node, on their own lines.
    Leading,
    /// Between the last element and a closing delimiter.
    Stragglers,
    /// On the node's last line, after the code.
    Trailing,
}

/// Comments grouped by owner, plus a sorted offset index for containment
/// queries.
pub(super) struct CommentTable {
    offsets: Vec<u32>,
    slots: HashMap<(Span, Slot), Vec<Comment>>,
}

impl CommentTable {
    /// Removes and returns the comments in a slot.
    pub(super) fn take(&mut self, key: Span, slot: Slot) -> Vec<Comment> {
        self.slots.remove(&(key, slot)).unwrap_or_default()
    }

    /// True when the slot holds at least one comment.
    pub(super) fn has(&self, key: Span, slot: Slot) -> bool {
        self.slots.contains_key(&(key, slot))
    }

    /// True when any comment starts strictly inside the span.
    pub(super) fn any_within(&self, span: Span) -> bool {
        let from = self.offsets.partition_point(|&o| o <= span.start.offset);
        self.offsets.get(from).is_some_and(|&o| o < span.end.offset)
    }

    /// Removes every unconsumed comment, sorted by source position. A
    /// non-empty result after printing is a printer bug. The caller emits
    /// them at the end of the file so nothing is lost, and debug builds
    /// assert.
    pub(super) fn drain_remaining(&mut self) -> Vec<Comment> {
        let mut rest: Vec<Comment> = self.slots.drain().flat_map(|(_, cs)| cs).collect();
        rest.sort_by_key(|c| c.span.start.offset);
        rest
    }
}

/// Builds the comment table for a parsed file.
pub(super) fn attach(file: &File, tokens: &[Token]) -> CommentTable {
    let mut attacher = Attacher {
        comments: &file.comments,
        pos: 0,
        table: CommentTable {
            offsets: file.comments.iter().map(|c| c.span.start.offset).collect(),
            slots: HashMap::new(),
        },
        tokens,
    };
    attacher.walk_file(file);
    attacher.table
}

struct Attacher<'a> {
    comments: &'a [Comment],
    pos: usize,
    table: CommentTable,
    tokens: &'a [Token],
}

/// A sequence child as the walk sees it, carrying the table key the
/// printer will look up, the offset its leading comments drain to, and
/// its span.
struct ChildInfo {
    key: Span,
    lead_offset: u32,
    span: Span,
}

impl ChildInfo {
    fn of(span: Span) -> Self {
        ChildInfo {
            key: span,
            lead_offset: span.start.offset,
            span,
        }
    }
}

impl<'a> Attacher<'a> {
    fn peek(&self) -> Option<&'a Comment> {
        self.comments.get(self.pos)
    }

    /// True when an unconsumed comment starts before `offset`.
    fn pending_before(&self, offset: u32) -> bool {
        self.peek().is_some_and(|c| c.span.start.offset < offset)
    }

    fn push(&mut self, key: Span, slot: Slot, comments: Vec<Comment>) {
        if comments.is_empty() {
            return;
        }
        self.table
            .slots
            .entry((key, slot))
            .or_default()
            .extend(comments);
    }

    /// Consumes comments starting before `offset`.
    fn take_before(&mut self, offset: u32) -> Vec<Comment> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if c.span.start.offset >= offset {
                break;
            }
            out.push(c.clone());
            self.pos += 1;
        }
        out
    }

    /// Consumes comments before `offset` until a blank line starts a new
    /// comment run.
    fn take_before_without_blank(&mut self, offset: u32, previous_line: u32) -> Vec<Comment> {
        let mut out = Vec::new();
        let mut last_line = previous_line;
        while let Some(comment) = self.peek() {
            if comment.span.start.offset >= offset || comment.span.start.line > last_line + 1 {
                break;
            }
            last_line = comment.span.end.line;
            out.push(comment.clone());
            self.pos += 1;
        }
        out
    }

    /// Consumes comments on exactly `line` that start before `bound`.
    fn take_on_line(&mut self, line: u32, bound: u32) -> Vec<Comment> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if c.span.start.line != line || c.span.start.offset >= bound {
                break;
            }
            out.push(c.clone());
            self.pos += 1;
        }
        out
    }

    /// The first token of `kind` in the offset range, if any.
    fn find_token(&self, kind: TokenKind, from: u32, to: u32) -> Option<&'a Token> {
        let start = self.tokens.partition_point(|t| t.span.start.offset < from);
        self.tokens[start..]
            .iter()
            .take_while(|t| t.span.start.offset < to)
            .find(|t| t.kind == kind)
    }

    // --- generic sequence walking ---

    /// Walks one sequence child: leading comments, interior, a safety
    /// sweep of unclaimed interior comments into `Leading`, and a
    /// trailing take on the child's last line. `next_offset` is where the
    /// following content starts (the next child, or the region bound).
    fn walk_child(&mut self, child: &ChildInfo, next_offset: u32, walk: impl FnOnce(&mut Self)) {
        let leading = self.take_before(child.lead_offset);
        self.push(child.key, Slot::Leading, leading);
        walk(self);
        // Comments the interior walk left behind relocate above the child
        // rather than leak into a sibling. Or-pattern separators are the
        // known case.
        let strays = self.take_before(child.span.end.offset);
        self.push(child.key, Slot::Leading, strays);
        let trailing = self.take_on_line(content_end_line(child.span), next_offset);
        self.push(child.key, Slot::Trailing, trailing);
    }

    /// Walks a uniform child slice, then routes region-final comments to
    /// `dangling`.
    fn walk_children<T>(
        &mut self,
        items: &[T],
        info: impl Fn(&T) -> ChildInfo,
        mut walk: impl FnMut(&mut Self, &T),
        bound: u32,
        dangling: (Span, Slot),
    ) {
        for (i, item) in items.iter().enumerate() {
            let child = info(item);
            let next_offset = items.get(i + 1).map_or(bound, |n| info(n).lead_offset);
            self.walk_child(&child, next_offset, |s| walk(s, item));
        }
        let rest = self.take_before(bound);
        self.push(dangling.0, dangling.1, rest);
    }

    /// Walks a statement body whose region-final comments dangle before
    /// the owner's terminator.
    fn walk_body(&mut self, stmts: &[Statement], bound: u32, owner: Span) {
        self.walk_children(
            stmts,
            |s| ChildInfo::of(stmt_span(s)),
            |a, s| a.walk_stmt(s),
            bound,
            (owner, Slot::Dangling),
        );
    }

    // --- file and items ---

    fn walk_file(&mut self, file: &File) {
        let mut children: Vec<(u32, TopChild<'_>)> = Vec::new();
        for item in &file.items {
            children.push((item_lead_offset(item), TopChild::Item(item)));
        }
        if let Some(body) = &file.body {
            for stmt in body {
                children.push((stmt_span(stmt).start.offset, TopChild::Stmt(stmt)));
            }
        }
        children.sort_by_key(|(offset, _)| *offset);

        for (i, (_, node)) in children.iter().enumerate() {
            let child = match node {
                TopChild::Item(item) => item_child_info(item),
                TopChild::Stmt(stmt) => ChildInfo::of(stmt_span(stmt)),
            };
            let next_offset = children.get(i + 1).map_or(u32::MAX, |(o, _)| *o);
            match node {
                TopChild::Item(item) => {
                    self.walk_child(&child, next_offset, |s| s.walk_item(item));
                }
                TopChild::Stmt(stmt) => {
                    self.walk_child(&child, next_offset, |s| s.walk_stmt(stmt));
                }
            }
        }
        let rest = self.take_before(u32::MAX);
        self.push(file.span, Slot::Dangling, rest);
    }

    fn walk_item(&mut self, item: &Item) {
        // Comments between an annotation and its declaration hoist above
        // the annotation, so they join the item's leading run.
        let hoisted = self.take_before(item_span(item).start.offset);
        self.push(*item_span(item), Slot::Leading, hoisted);
        match item {
            Item::Alias(_) => {}
            Item::Builtin(b) => self.walk_decl_body(
                b.span,
                b.span.start.line,
                b.functions.iter().map(Member::Function).collect(),
            ),
            Item::Constant(c) => self.walk_expr(&c.value),
            Item::Enum(e) => {
                let members = e
                    .variants
                    .iter()
                    .map(Member::Variant)
                    .chain(e.nested.iter().map(Member::Nested))
                    .chain(e.functions.iter().map(Member::Function))
                    .collect();
                self.walk_decl_body(e.span, header_end_line(e.span, &e.conformances), members);
            }
            Item::Extend(e) => self.walk_decl_body(
                e.span,
                header_end_line_impl(&e.target, None, e.span),
                impl_members(&e.members),
            ),
            Item::Function(f) => self.walk_function(f),
            Item::Impl(i) => self.walk_decl_body(
                i.span,
                header_end_line_impl(&i.target, Some(&i.trait_expr), i.span),
                impl_members(&i.members),
            ),
            Item::Protocol(p) => self.walk_decl_body(
                p.span,
                p.span.start.line,
                p.methods.iter().map(Member::ProtocolMethod).collect(),
            ),
            Item::Struct(s) => {
                let members = s
                    .fields
                    .iter()
                    .map(Member::Field)
                    .chain(s.nested.iter().map(Member::Nested))
                    .chain(s.functions.iter().map(Member::Function))
                    .collect();
                self.walk_decl_body(s.span, header_end_line(s.span, &s.conformances), members);
            }
            Item::TypeAlias(_) => {}
        }
    }

    /// Walks any declaration body. Takes the comment trailing the header
    /// line, walks the members merged in source order, and dangles
    /// region-final comments before `end`.
    fn walk_decl_body(&mut self, decl_span: Span, header_end: u32, mut members: Vec<Member<'_>>) {
        members.sort_by_key(|m| m.child_info().lead_offset);
        let first_member = members
            .first()
            .map_or(decl_span.end.offset, |m| m.child_info().lead_offset);
        let trailing = self.take_on_line(header_end, first_member);
        self.push(decl_span, Slot::HeaderTrailing, trailing);

        for (i, member) in members.iter().enumerate() {
            let next_offset = members
                .get(i + 1)
                .map_or(decl_span.end.offset, |m| m.child_info().lead_offset);
            self.walk_child(&member.child_info(), next_offset, |s| member.walk(s));
        }
        let rest = self.take_before(decl_span.end.offset);
        self.push(decl_span, Slot::Dangling, rest);
    }

    fn walk_variant(&mut self, v: &EnumVariant) {
        if let EnumVariantData::Struct(fields) = &v.data {
            self.walk_children(
                fields,
                |f| ChildInfo::of(f.span),
                |a, f| a.walk_field_default(f),
                v.span.end.offset,
                (v.span, Slot::Stragglers),
            );
        }
    }

    fn walk_field_default(&mut self, field: &StructField) {
        if let Some(default) = &field.default {
            self.walk_expr(default);
        }
    }

    fn walk_protocol_method(&mut self, m: &ProtocolMethod) {
        let hoisted = self.take_before(m.span.start.offset);
        self.push(m.span, Slot::Leading, hoisted);
        self.walk_signature(
            m.span,
            &m.params,
            m.return_type.as_ref(),
            m.error_type.as_ref(),
            m.body.as_deref(),
        );
        if let Some(body) = &m.body {
            self.walk_body(body, m.span.end.offset, m.span);
        }
    }

    fn walk_function(&mut self, f: &Function) {
        let hoisted = self.take_before(f.span.start.offset);
        self.push(f.span, Slot::Leading, hoisted);
        self.walk_signature(
            f.span,
            &f.params,
            f.return_type.as_ref(),
            f.error_type.as_ref(),
            f.body.as_deref(),
        );
        if let Some(body) = &f.body {
            self.walk_body(body, f.span.end.offset, f.span);
        }
    }

    /// Walks a function or protocol-method signature: per-parameter
    /// comments, stragglers before the closing paren, and the trailing
    /// comment on the signature's last line.
    fn walk_signature(
        &mut self,
        owner: Span,
        params: &[Param],
        return_type: Option<&TypeExpr>,
        error_type: Option<&TypeExpr>,
        body: Option<&[Statement]>,
    ) {
        let after_sig = body
            .and_then(|b| b.first())
            .map(|s| stmt_span(s).start.offset)
            .unwrap_or(owner.end.offset);
        // A wrapped parameter list puts the bare `)` on its own line. The
        // signature reaches that line, so a comment there stays a
        // header-trailing comment (`) # c`) and the form is idempotent.
        let paren_line = params
            .last()
            .and_then(|p| self.find_token(TokenKind::RParen, param_span(p).end.offset, after_sig))
            .map_or(0, |t| t.span.start.line);
        let sig_end =
            signature_end_line(owner.start.line, params, return_type, error_type).max(paren_line);

        for (i, param) in params.iter().enumerate() {
            let span = *param_span(param);
            let next_offset = params
                .get(i + 1)
                .map_or(after_sig, |p| param_span(p).start.offset);
            let leading = self.take_before(span.start.offset);
            self.push(span, Slot::Leading, leading);
            if let Param::Regular {
                default: Some(d), ..
            } = param
            {
                self.walk_expr(d);
            }
            // A comment on the signature's last line trails the whole
            // signature, not the parameter that happens to end there.
            if span.end.line < sig_end {
                let trailing = self.take_on_line(span.end.line, next_offset);
                self.push(span, Slot::Trailing, trailing);
            }
        }
        // Comments below the last parameter but above the signature's end
        // line sit before the closing paren.
        let mut stragglers = Vec::new();
        while let Some(c) = self.peek() {
            if c.span.start.line >= sig_end || c.span.start.offset >= after_sig {
                break;
            }
            stragglers.push(c.clone());
            self.pos += 1;
        }
        self.push(owner, Slot::Stragglers, stragglers);
        let trailing = self.take_on_line(sig_end, after_sig);
        self.push(owner, Slot::HeaderTrailing, trailing);
    }

    // --- statements and expressions ---

    fn walk_stmt(&mut self, stmt: &Statement) {
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

    fn walk_expr(&mut self, expr: &Expr) {
        if !self.pending_before(expr.span.end.offset) {
            return;
        }
        match &expr.kind {
            ExprKind::Ident { .. } | ExprKind::Literal { .. } | ExprKind::Self_ { .. } => {}

            ExprKind::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::Group { expr: inner }
            | ExprKind::Spawn { expr: inner }
            | ExprKind::Try { expr: inner } => self.walk_expr(inner),
            ExprKind::Fail { value } => self.walk_expr(value),
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
                let first_stmt = body
                    .first()
                    .map(|s| stmt_span(s).start.offset)
                    .unwrap_or(expr.span.end.offset);
                let trailing = self.take_on_line(expr.span.start.line, first_stmt);
                self.push(expr.span, Slot::HeaderTrailing, trailing);
                self.walk_body(body, expr.span.end.offset, expr.span);
            }

            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                self.walk_expr(condition);
                let first_then = then_body
                    .first()
                    .map(|s| stmt_span(s).start.offset)
                    .unwrap_or(expr.span.end.offset);
                let trailing = self.take_on_line(condition.span.end.line, first_then);
                self.push(expr.span, Slot::HeaderTrailing, trailing);
                match else_body {
                    Some(else_stmts) => {
                        let search_from = then_body
                            .last()
                            .map(|s| stmt_span(s).end.offset)
                            .unwrap_or(condition.span.end.offset);
                        let else_token =
                            self.find_token(TokenKind::Else, search_from, expr.span.end.offset);
                        let else_offset =
                            else_token.map_or(expr.span.end.offset, |t| t.span.start.offset);
                        let else_line = else_token.map(|t| t.span.start.line);
                        self.walk_body(then_body, else_offset, expr.span);
                        let before_else = self.table.take(expr.span, Slot::Dangling);
                        self.push(expr.span, Slot::BeforeElse, before_else);
                        if let Some(line) = else_line {
                            let first_else = else_stmts
                                .first()
                                .map(|s| stmt_span(s).start.offset)
                                .unwrap_or(expr.span.end.offset);
                            let trailing = self.take_on_line(line, first_else);
                            self.push(expr.span, Slot::ElseTrailing, trailing);
                        }
                        self.walk_body(else_stmts, expr.span.end.offset, expr.span);
                    }
                    None => self.walk_body(then_body, expr.span.end.offset, expr.span),
                }
            }

            ExprKind::Unless { condition, body } | ExprKind::While { condition, body } => {
                self.walk_expr(condition);
                let first_stmt = body
                    .first()
                    .map(|s| stmt_span(s).start.offset)
                    .unwrap_or(expr.span.end.offset);
                let trailing = self.take_on_line(condition.span.end.line, first_stmt);
                self.push(expr.span, Slot::HeaderTrailing, trailing);
                self.walk_body(body, expr.span.end.offset, expr.span);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
            } => {
                self.walk_pattern(pattern);
                self.walk_expr(iterable);
                let first_stmt = body
                    .first()
                    .map(|s| stmt_span(s).start.offset)
                    .unwrap_or(expr.span.end.offset);
                let trailing = self.take_on_line(iterable.span.end.line, first_stmt);
                self.push(expr.span, Slot::HeaderTrailing, trailing);
                self.walk_body(body, expr.span.end.offset, expr.span);
            }
            ExprKind::Loop { body } => {
                let first_stmt = body
                    .first()
                    .map(|s| stmt_span(s).start.offset)
                    .unwrap_or(expr.span.end.offset);
                let trailing = self.take_on_line(expr.span.start.line, first_stmt);
                self.push(expr.span, Slot::HeaderTrailing, trailing);
                self.walk_body(body, expr.span.end.offset, expr.span);
            }

            ExprKind::Match { subject, arms } => {
                self.walk_expr(subject);
                let first_arm = arms
                    .first()
                    .map(|a| a.span.start.offset)
                    .unwrap_or(expr.span.end.offset);
                let trailing = self.take_on_line(subject.span.end.line, first_arm);
                self.push(expr.span, Slot::HeaderTrailing, trailing);
                self.walk_match_arms(arms, expr.span.end.offset, expr.span, true);
                let rest = self.take_before(expr.span.end.offset);
                self.push(expr.span, Slot::Dangling, rest);
            }

            ExprKind::Cond { arms, else_body } => {
                let first_arm = arms
                    .first()
                    .map(|a| a.span.start.offset)
                    .unwrap_or(expr.span.end.offset);
                let trailing = self.take_on_line(expr.span.start.line, first_arm);
                self.push(expr.span, Slot::HeaderTrailing, trailing);

                let arms_end = match else_body {
                    Some(_) => {
                        let search_from = arms
                            .last()
                            .map(|a| a.span.end.offset)
                            .unwrap_or(expr.span.start.offset);
                        self.find_token(TokenKind::Else, search_from, expr.span.end.offset)
                            .map_or(expr.span.end.offset, |t| t.span.start.offset)
                    }
                    None => expr.span.end.offset,
                };
                for (i, arm) in arms.iter().enumerate() {
                    let is_last = i + 1 == arms.len() && else_body.is_none();
                    let next_offset = arms.get(i + 1).map_or(arms_end, |a| a.span.start.offset);
                    self.walk_cond_arm(arm, is_last, next_offset);
                }
                match else_body {
                    Some(else_stmts) => {
                        let before_else = self.take_before(arms_end);
                        self.push(expr.span, Slot::BeforeElse, before_else);
                        let else_line = self
                            .find_token(TokenKind::Else, arms_end, expr.span.end.offset)
                            .map(|t| t.span.start.line);
                        if let Some(line) = else_line {
                            let first_else = else_stmts
                                .first()
                                .map(|s| stmt_span(s).start.offset)
                                .unwrap_or(expr.span.end.offset);
                            let trailing = self.take_on_line(line, first_else);
                            self.push(expr.span, Slot::ElseTrailing, trailing);
                        }
                        self.walk_body(else_stmts, expr.span.end.offset, expr.span);
                    }
                    None => {
                        let rest = self.take_before(expr.span.end.offset);
                        self.push(expr.span, Slot::Dangling, rest);
                    }
                }
            }

            ExprKind::Receive {
                arms,
                after_timeout,
                after_body,
            } => {
                let first_arm = arms
                    .first()
                    .map(|a| a.span.start.offset)
                    .unwrap_or(expr.span.end.offset);
                let trailing = self.take_on_line(expr.span.start.line, first_arm);
                self.push(expr.span, Slot::HeaderTrailing, trailing);

                let arms_end = match after_timeout {
                    Some(_) => {
                        let search_from = arms
                            .last()
                            .map(|a| a.span.end.offset)
                            .unwrap_or(expr.span.start.offset);
                        self.find_token(TokenKind::After, search_from, expr.span.end.offset)
                            .map_or(expr.span.end.offset, |t| t.span.start.offset)
                    }
                    None => expr.span.end.offset,
                };
                self.walk_match_arms(arms, arms_end, expr.span, after_timeout.is_none());
                if let Some(timeout) = after_timeout {
                    // The arms walk routed the boundary comments to
                    // Dangling. With an `after` clause they sit above it.
                    let before_after = self.table.take(expr.span, Slot::Dangling);
                    self.push(expr.span, Slot::BeforeAfter, before_after);
                    self.walk_expr(timeout);
                    let first_stmt = after_body
                        .first()
                        .map(|s| stmt_span(s).start.offset)
                        .unwrap_or(expr.span.end.offset);
                    let trailing = self.take_on_line(timeout.span.end.line, first_stmt);
                    self.push(timeout.span, Slot::HeaderTrailing, trailing);
                    self.walk_body(after_body, expr.span.end.offset, expr.span);
                } else {
                    let rest = self.take_before(expr.span.end.offset);
                    self.push(expr.span, Slot::Dangling, rest);
                }
            }
        }
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
        let first_stmt = body
            .first()
            .map(|s| stmt_span(s).start.offset)
            .unwrap_or(arm_span.end.offset);
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
        let mut links: Vec<&Expr> = Vec::new();
        let mut current = expr;
        while let ExprKind::MethodCall { receiver, .. } = &current.kind {
            links.push(current);
            current = receiver;
        }
        links.reverse();
        self.walk_expr(current);

        for (i, link) in links.iter().enumerate() {
            let ExprKind::MethodCall { args, receiver, .. } = &link.kind else {
                unreachable!()
            };
            // Only real chains (2+ links) render link-leading comments;
            // a single call routes them into its argument list instead.
            if links.len() > 1 {
                let lead_offset = args
                    .first()
                    .map_or(link.span.end.offset, |a| a.span.start.offset);
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

enum TopChild<'a> {
    Item(&'a Item),
    Stmt(&'a Statement),
}

/// One member of a declaration body, unified across every declaration
/// kind so [`Attacher::walk_decl_body`] can walk them all the same way.
enum Member<'a> {
    Field(&'a StructField),
    Function(&'a Function),
    Nested(&'a Item),
    ProtocolMethod(&'a ProtocolMethod),
    TypeAlias(&'a TypeAlias),
    Variant(&'a EnumVariant),
}

impl Member<'_> {
    fn child_info(&self) -> ChildInfo {
        match self {
            Member::Field(f) => ChildInfo::of(f.span),
            Member::Function(f) => function_child_info(f),
            Member::Nested(n) => item_child_info(n),
            Member::ProtocolMethod(m) => ChildInfo {
                key: m.span,
                lead_offset: m
                    .annotations
                    .first()
                    .map_or(m.span.start.offset, |a| a.span.start.offset),
                span: m.span,
            },
            Member::TypeAlias(t) => ChildInfo::of(t.span),
            Member::Variant(v) => ChildInfo::of(v.span),
        }
    }

    fn walk(&self, attacher: &mut Attacher<'_>) {
        match self {
            Member::Field(f) => attacher.walk_field_default(f),
            Member::Function(f) => attacher.walk_function(f),
            Member::Nested(n) => attacher.walk_item(n),
            Member::ProtocolMethod(m) => attacher.walk_protocol_method(m),
            Member::TypeAlias(_) => {}
            Member::Variant(v) => attacher.walk_variant(v),
        }
    }
}

fn impl_members(members: &[ImplMember]) -> Vec<Member<'_>> {
    members
        .iter()
        .map(|m| match m {
            ImplMember::Function(f) => Member::Function(f),
            ImplMember::TypeAlias(t) => Member::TypeAlias(t),
        })
        .collect()
}

/// Sequence-child info for an item, keyed by its declaration span with
/// leading comments draining to the first annotation.
fn item_child_info(item: &Item) -> ChildInfo {
    ChildInfo {
        key: *item_span(item),
        lead_offset: item_lead_offset(item),
        span: *item_span(item),
    }
}

fn function_child_info(f: &Function) -> ChildInfo {
    ChildInfo {
        key: f.span,
        lead_offset: item_lead_offset_fn(f),
        span: f.span,
    }
}

/// The last line holding the node's content. Some parser spans end at
/// column 1 of the following line (bodyless signatures, inline arms), and
/// a trailing take keyed on that line would steal the next line's
/// comments.
fn content_end_line(span: Span) -> u32 {
    if span.end.column == 1 && span.end.line > span.start.line {
        span.end.line - 1
    } else {
        span.end.line
    }
}

/// First offset of an item including its annotations.
fn item_lead_offset(item: &Item) -> u32 {
    item_annotations(item)
        .first()
        .map(|a| a.span.start.offset)
        .unwrap_or_else(|| item_span(item).start.offset)
}

fn item_lead_offset_fn(f: &Function) -> u32 {
    f.annotations
        .first()
        .map_or(f.span.start.offset, |a| a.span.start.offset)
}

/// Last line of a struct/enum header, extended by a wrapped conformance
/// list.
fn header_end_line(decl_span: Span, conformances: &[TypeExpr]) -> u32 {
    conformances
        .iter()
        .map(|c| koja_ast::labels::type_expr_span(c).end.line)
        .max()
        .unwrap_or(decl_span.start.line)
        .max(decl_span.start.line)
}

/// Last line of an `impl`/`extend` header.
fn header_end_line_impl(target: &TypeExpr, trait_expr: Option<&TypeExpr>, decl_span: Span) -> u32 {
    let target_line = koja_ast::labels::type_expr_span(target).end.line;
    let trait_line = trait_expr.map_or(0, |t| koja_ast::labels::type_expr_span(t).end.line);
    target_line.max(trait_line).max(decl_span.start.line)
}
