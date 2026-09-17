//! The file and declaration walks: items, declaration bodies, and
//! signatures.

use koja_ast::ast::*;
use koja_ast::labels::type_expr_span;
use koja_ast::span::Span;
use koja_ast::token::TokenKind;

use super::super::util::{
    TopLevel, item_lead_offset, item_span, param_span, signature_end_line, top_level_nodes,
};
use super::{Attacher, ChildInfo, Slot, first_stmt_offset};

impl Attacher<'_> {
    pub(super) fn walk_file(&mut self, file: &File) {
        let nodes = top_level_nodes(file);
        for (i, node) in nodes.iter().enumerate() {
            let child = ChildInfo {
                key: node.key(),
                lead_offset: node.lead_offset(),
                span: node.key(),
            };
            let next_offset = nodes.get(i + 1).map_or(u32::MAX, TopLevel::lead_offset);
            self.walk_child(&child, next_offset, |s| match node {
                TopLevel::Item(item) => s.walk_item(item),
                TopLevel::Stmt(stmt) => s.walk_stmt(stmt),
            });
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
                b.nested
                    .iter()
                    .map(Member::Nested)
                    .chain(b.functions.iter().map(Member::Function))
                    .chain(b.tests.iter().map(Member::Test))
                    .collect(),
            ),
            Item::Constant(c) => self.walk_expr(&c.value),
            Item::Enum(e) => {
                let members = e
                    .variants
                    .iter()
                    .map(Member::Variant)
                    .chain(e.nested.iter().map(Member::Nested))
                    .chain(e.functions.iter().map(Member::Function))
                    .chain(e.tests.iter().map(Member::Test))
                    .collect();
                self.walk_decl_body(e.span, header_end_line(e.span, &e.conformances), members);
            }
            Item::Extend(e) => self.walk_decl_body(
                e.span,
                header_end_line_impl(&e.target, None, e.span),
                impl_members(&e.members, &e.tests),
            ),
            Item::Function(f) => self.walk_function(f),
            Item::Impl(i) => self.walk_decl_body(
                i.span,
                header_end_line_impl(&i.target, Some(&i.trait_expr), i.span),
                impl_members(&i.members, &i.tests),
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
                    .chain(s.tests.iter().map(Member::Test))
                    .collect();
                self.walk_decl_body(s.span, header_end_line(s.span, &s.conformances), members);
            }
            Item::Test(t) => self.walk_test(t),
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
        self.take_header_trailing(decl_span, header_end, first_member);

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
        let after_sig = first_stmt_offset(body.unwrap_or_default(), owner.end.offset);
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
        self.take_header_trailing(owner, sig_end, after_sig);
    }

    fn walk_test(&mut self, t: &TestDecl) {
        let hoisted = self.take_before(t.span.start.offset);
        self.push(t.span, Slot::Leading, hoisted);
        self.walk_block(t.span, t.span.start.line, &t.body);
    }
}

/// One member of a declaration body, unified across every declaration
/// kind so [`Attacher::walk_decl_body`] can walk them all the same way.
enum Member<'a> {
    Field(&'a StructField),
    Function(&'a Function),
    Nested(&'a Item),
    ProtocolMethod(&'a ProtocolMethod),
    Test(&'a TestDecl),
    TypeAlias(&'a TypeAlias),
    Variant(&'a EnumVariant),
}

impl Member<'_> {
    fn child_info(&self) -> ChildInfo {
        match self {
            Member::Field(f) => ChildInfo::of(f.span),
            Member::Function(f) => ChildInfo::annotated(f.span, &f.annotations),
            Member::Nested(n) => ChildInfo {
                key: *item_span(n),
                lead_offset: item_lead_offset(n),
                span: *item_span(n),
            },
            Member::ProtocolMethod(m) => ChildInfo::annotated(m.span, &m.annotations),
            Member::Test(t) => ChildInfo::of(t.span),
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
            Member::Test(t) => attacher.walk_test(t),
            Member::TypeAlias(_) => {}
            Member::Variant(v) => attacher.walk_variant(v),
        }
    }
}

fn impl_members<'a>(members: &'a [ImplMember], tests: &'a [TestDecl]) -> Vec<Member<'a>> {
    members
        .iter()
        .map(|m| match m {
            ImplMember::Function(f) => Member::Function(f),
            ImplMember::TypeAlias(t) => Member::TypeAlias(t),
        })
        .chain(tests.iter().map(Member::Test))
        .collect()
}

/// Last line of a struct/enum header, extended by a wrapped conformance
/// list.
fn header_end_line(decl_span: Span, conformances: &[TypeExpr]) -> u32 {
    conformances
        .iter()
        .map(|c| type_expr_span(c).end.line)
        .max()
        .unwrap_or(decl_span.start.line)
        .max(decl_span.start.line)
}

/// Last line of an `impl`/`extend` header.
fn header_end_line_impl(target: &TypeExpr, trait_expr: Option<&TypeExpr>, decl_span: Span) -> u32 {
    let target_line = type_expr_span(target).end.line;
    let trait_line = trait_expr.map_or(0, |t| type_expr_span(t).end.line);
    target_line.max(trait_line).max(decl_span.start.line)
}
