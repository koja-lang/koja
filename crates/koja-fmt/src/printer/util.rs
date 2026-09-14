//! Pure utility functions for the formatter.
//!
//! Everything here is stateless: no comment cursor, no `&mut self`. These
//! convert AST fragments (types, patterns, literals, imports, annotations)
//! into `Doc` nodes, and provide span and text-length helpers used by the
//! printer and expression modules.

use crate::doc::*;
use koja_ast::ast::*;
use koja_ast::labels::type_expr_span;
use koja_ast::span::Span;

use super::Printer;

/// Formats a `TypeParam` as a string, including bounds if present.
/// E.g. `T`, `T: Debug`, `T: Debug & Hash`.
pub(super) fn format_type_param(tp: &TypeParam) -> String {
    if tp.bounds.is_empty() {
        tp.name.clone()
    } else {
        let bounds = tp
            .bounds
            .iter()
            .map(|bound| render(&type_expr_to_doc(bound), u32::MAX))
            .collect::<Vec<_>>()
            .join(" & ");
        format!("{}: {bounds}", tp.name)
    }
}

pub(super) fn format_type_params(tps: &[TypeParam]) -> String {
    tps.iter()
        .map(format_type_param)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Formats a `<T: Bound, U>` list with the same break shape as a
/// parameter list. Each entry stays atomic, including its bounds.
/// Returns `Nil` for an empty list.
pub(super) fn type_params_doc(tps: &[TypeParam]) -> Doc {
    if tps.is_empty() {
        return nil();
    }
    let items: Vec<Doc> = tps.iter().map(|tp| text(format_type_param(tp))).collect();
    group(concat(vec![
        text("<"),
        indent(
            2,
            concat(vec![
                softline(),
                intersperse(items, concat(vec![text(","), line()])),
            ]),
        ),
        softline(),
        text(">"),
    ]))
}

/// Formats a comma-separated list of items using fill layout inside brackets.
///
/// Items are packed left-to-right on each line. A trailing comma is added
/// to all items except the last. The result is wrapped in a group so the
/// whole list can collapse to a single line when it fits.
pub(super) fn fill_bracket_list(open: &str, close: &str, items: Vec<Doc>) -> Doc {
    group(bracket_list_body(open, close, items))
}

/// The layout of [`fill_bracket_list`] without its enclosing group, so a
/// caller can bind the break decision to a larger group.
pub(super) fn bracket_list_body(open: &str, close: &str, items: Vec<Doc>) -> Doc {
    let last = items.len() - 1;
    let fill_items: Vec<Doc> = items
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            if i < last {
                concat(vec![d, text(",")])
            } else {
                d
            }
        })
        .collect();
    concat(vec![
        text(open),
        indent(2, concat(vec![softline(), fill(fill_items)])),
        softline(),
        text(close),
    ])
}

/// Formats a conformance header (`: Display, Hash`) for a struct or
/// enum declaration. Collapses onto the header line when it fits,
/// otherwise breaks after the colon with fill-packed entries and a
/// blank line separating the list from the body, matching wrapped
/// function signatures.
pub(super) fn conformance_header_doc(conformances: &[TypeExpr]) -> Doc {
    let last = conformances.len() - 1;
    let fill_items: Vec<Doc> = conformances
        .iter()
        .map(type_expr_to_doc)
        .enumerate()
        .map(|(i, d)| {
            if i < last {
                concat(vec![d, text(",")])
            } else {
                d
            }
        })
        .collect();
    group(concat(vec![
        text(":"),
        indent(2, concat(vec![line(), fill(fill_items)])),
        if_break(nil(), hardline()),
    ]))
}

/// Formats a struct-like body: `prefix{ field, field, ... }` with
/// trailing-comma layout that breaks across lines when needed.
pub(super) fn struct_body(prefix: Doc, field_docs: Vec<Doc>) -> Doc {
    concat(vec![prefix, delimited_list("{", "}", field_docs)])
}

/// A comma-separated list between `open` and `close` that stays on one
/// line when it fits and otherwise puts one item per line with a
/// trailing comma. The shape of parameter lists, call arguments, and
/// struct bodies.
pub(super) fn delimited_list(open: &str, close: &str, items: Vec<Doc>) -> Doc {
    group(concat(vec![
        text(open),
        indent(
            2,
            concat(vec![
                softline(),
                intersperse(items, concat(vec![text(","), line()])),
                trailing_comma(),
            ]),
        ),
        softline(),
        text(close),
    ]))
}

/// `interior` between `open` and `close`, always broken: the interior
/// is indented and `close` sits on its own line. `interior` supplies
/// its own leading line break.
pub(super) fn broken_list(open: &str, close: &str, interior: Doc) -> Doc {
    concat(vec![
        text(open),
        indent(2, interior),
        hardline(),
        text(close),
    ])
}

/// A single top-level element, either a declaration or a statement.
/// `.kojs` scripts carry statements in `file.body` next to `file.items`,
/// and both the attach pass and the printer walk them merged back into
/// source order.
pub(super) enum TopLevel<'a> {
    Item(&'a Item),
    Stmt(&'a Statement),
}

impl TopLevel<'_> {
    /// First source line as authored, annotations included.
    pub(super) fn start_line(&self) -> u32 {
        match self {
            TopLevel::Item(item) => item_start_line(item),
            TopLevel::Stmt(stmt) => stmt_span(stmt).start.line,
        }
    }

    /// First source offset as authored, annotations included.
    pub(super) fn lead_offset(&self) -> u32 {
        match self {
            TopLevel::Item(item) => item_lead_offset(item),
            TopLevel::Stmt(stmt) => stmt_span(stmt).start.offset,
        }
    }

    /// The span that keys this node's comments. For items that is the
    /// declaration span with annotations excluded.
    pub(super) fn key(&self) -> Span {
        match self {
            TopLevel::Item(item) => *item_span(item),
            TopLevel::Stmt(stmt) => stmt_span(stmt),
        }
    }

    /// Whether this element forces blank-line separation from its
    /// neighbors. Multi-line declarations, annotated declarations, and
    /// block statements read better with surrounding blank lines, while
    /// bare single-line `const`/`alias` declarations flow with adjacent
    /// statements.
    pub(super) fn is_block(&self) -> bool {
        match self {
            TopLevel::Item(item @ (Item::Constant(_) | Item::Alias(_) | Item::TypeAlias(_))) => {
                !item_annotations(item).is_empty()
            }
            TopLevel::Item(_) => true,
            TopLevel::Stmt(stmt) => stmt_is_block(stmt),
        }
    }
}

/// The file's items and script statements merged into source order.
pub(super) fn top_level_nodes(file: &File) -> Vec<TopLevel<'_>> {
    let mut nodes: Vec<TopLevel<'_>> = file.items.iter().map(TopLevel::Item).collect();
    if let Some(body) = &file.body {
        nodes.extend(body.iter().map(TopLevel::Stmt));
    }
    nodes.sort_by_key(TopLevel::lead_offset);
    nodes
}

pub(super) fn item_span(item: &Item) -> &Span {
    match item {
        Item::Alias(a) => &a.span,
        Item::Builtin(b) => &b.span,
        Item::Constant(c) => &c.span,
        Item::Enum(e) => &e.span,
        Item::Extend(e) => &e.span,
        Item::Function(f) => &f.span,
        Item::Impl(i) => &i.span,
        Item::Protocol(p) => &p.span,
        Item::Struct(s) => &s.span,
        Item::Test(t) => &t.span,
        Item::TypeAlias(t) => &t.span,
    }
}

/// The item's leading annotations (`@doc`, `@extern`, ...), empty for
/// item kinds that cannot carry any.
pub(super) fn item_annotations(item: &Item) -> &[Annotation] {
    match item {
        Item::Alias(_) | Item::Extend(_) | Item::Impl(_) | Item::Test(_) => &[],
        Item::Builtin(b) => &b.annotations,
        Item::Constant(c) => &c.annotations,
        Item::Enum(e) => &e.annotations,
        Item::Function(f) => &f.annotations,
        Item::Protocol(p) => &p.annotations,
        Item::Struct(s) => &s.annotations,
        Item::TypeAlias(t) => &t.annotations,
    }
}

/// First source line of an item including leading annotations, which
/// sit outside the declaration span. Blank-line detection between
/// items must measure from here, not from the declaration keyword.
pub(super) fn item_start_line(item: &Item) -> u32 {
    lead_line(item_annotations(item), *item_span(item))
}

/// First source offset of an item including leading annotations.
pub(super) fn item_lead_offset(item: &Item) -> u32 {
    lead_offset(item_annotations(item), *item_span(item))
}

/// First source line of a declaration whose annotations sit outside
/// its span.
pub(super) fn lead_line(annotations: &[Annotation], span: Span) -> u32 {
    annotations
        .first()
        .map_or(span.start.line, |a| a.span.start.line)
}

/// First source offset of a declaration whose annotations sit outside
/// its span.
pub(super) fn lead_offset(annotations: &[Annotation], span: Span) -> u32 {
    annotations
        .first()
        .map_or(span.start.offset, |a| a.span.start.offset)
}

/// The `priv ` keyword prefix for a private declaration, empty for
/// public ones.
pub(super) fn visibility_prefix(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Private => "priv ",
        Visibility::Public => "",
    }
}

pub(super) fn alias_to_doc(a: &AliasDecl) -> Doc {
    let mut parts = Vec::new();
    parts.push(text("alias "));
    parts.push(text(a.path.join(".")));
    let default_name = a.path.last().map(|s| s.as_str()).unwrap_or("");
    if a.local_name != default_name {
        parts.push(text(" as "));
        parts.push(text(&a.local_name));
    }
    concat(parts)
}

pub(super) fn type_alias_to_doc(t: &TypeAlias) -> Doc {
    let mut parts = Vec::new();
    push_annotations(&mut parts, &t.annotations);
    parts.push(text(visibility_prefix(t.visibility)));
    parts.push(text("type "));
    parts.push(text(&t.name.text));
    parts.push(text(" = "));
    parts.push(type_expr_to_doc(&t.type_expr));
    concat(parts)
}

/// Appends a declaration's annotations and the line break that
/// separates them from the declaration. Nothing when there are none.
pub(super) fn push_annotations(parts: &mut Vec<Doc>, annotations: &[Annotation]) {
    if let Some(doc) = annotations_to_doc(annotations) {
        parts.push(doc);
        parts.push(hardline());
    }
}

/// Formats a list of annotations, preserving the stacked/inline layout.
/// Annotations on the same line are joined with a space, annotations on
/// separate lines with hardlines.
fn annotations_to_doc(annotations: &[Annotation]) -> Option<Doc> {
    if annotations.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for (i, ann) in annotations.iter().enumerate() {
        if i > 0 {
            let prev = &annotations[i - 1];
            if ann.span.start.line == prev.span.start.line {
                parts.push(text(" "));
            } else {
                parts.push(hardline());
            }
        }
        parts.push(annotation_to_doc(ann));
    }
    Some(concat(parts))
}

fn annotation_to_doc(ann: &Annotation) -> Doc {
    match &ann.value {
        Some(AnnotationValue::String(val)) => {
            if val.contains('\n') {
                concat(vec![
                    text(format!("@{} \"\"\"", ann.name)),
                    hardline(),
                    text(escape_multiline_literal(val.trim())),
                    hardline(),
                    text("\"\"\""),
                ])
            } else {
                text(format!("@{} \"{}\"", ann.name, escape_string_literal(val)))
            }
        }
        Some(AnnotationValue::False) => text(format!("@{} false", ann.name)),
        None => text(format!("@{}", ann.name)),
    }
}

/// Renders an impl target with its conditional bounds inlined on
/// the matching args (`List<T: Equality>`). Args without a bound
/// entry and non-generic targets render like any other type.
pub(super) fn impl_target_to_doc(target: &TypeExpr, target_bounds: &[TypeParam]) -> Doc {
    let TypeExpr::Generic { path, args, .. } = target else {
        return type_expr_to_doc(target);
    };
    if target_bounds.is_empty() {
        return type_expr_to_doc(target);
    }
    let args_doc: Vec<Doc> = args
        .iter()
        .map(|arg| match arg {
            TypeExpr::Named { path, .. } if path.len() == 1 => target_bounds
                .iter()
                .find(|tp| tp.name == path[0])
                .map(|tp| text(format_type_param(tp)))
                .unwrap_or_else(|| type_expr_to_doc(arg)),
            _ => type_expr_to_doc(arg),
        })
        .collect();
    concat(vec![
        text(path.join(".")),
        text("<"),
        intersperse(args_doc, text(", ")),
        text(">"),
    ])
}

pub(super) fn type_expr_to_doc(ty: &TypeExpr) -> Doc {
    match ty {
        TypeExpr::Named { path, .. } => text(path.join(".")),
        TypeExpr::Generic { path, args, .. } => {
            let args_doc: Vec<Doc> = args.iter().map(type_expr_to_doc).collect();
            concat(vec![
                text(path.join(".")),
                text("<"),
                intersperse(args_doc, text(", ")),
                text(">"),
            ])
        }
        TypeExpr::Unit { .. } => text("()"),
        TypeExpr::Self_ { .. } => text("Self"),
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            let params_doc: Vec<Doc> = params.iter().map(type_expr_to_doc).collect();
            concat(vec![
                text("fn ("),
                intersperse(params_doc, text(", ")),
                text(") -> "),
                type_expr_to_doc(return_type),
            ])
        }
        TypeExpr::Tuple { elements, .. } => {
            let parts: Vec<Doc> = elements.iter().map(type_expr_to_doc).collect();
            concat(vec![text("("), intersperse(parts, text(", ")), text(")")])
        }
        TypeExpr::Union { types, .. } => {
            // Packs like a symbolic operator chain: `|` ends the line
            // and the continuation indents 2.
            let last = types.len() - 1;
            let members: Vec<Doc> = types
                .iter()
                .map(type_expr_to_doc)
                .enumerate()
                .map(|(i, doc)| {
                    if i == last {
                        doc
                    } else {
                        concat(vec![doc, text(" |")])
                    }
                })
                .collect();
            indent(2, fill(members))
        }
    }
}

/// Formats a `-> T ! E` return signature tail, shared by function
/// and protocol-method signatures. `None` when there is nothing to
/// print (unit return, no error type). A unit return with an error
/// type prints the bare canonical form `! E`, so `-> () ! E`
/// normalizes on format.
///
/// The tail is its own group. When the surrounding signature breaks,
/// `-> T ! E` remains on one continuation line if it fits. Only the
/// tail's own overflow separates `-> T` from `! E`.
pub(super) fn return_signature_doc(
    return_type: Option<&TypeExpr>,
    error_type: Option<&TypeExpr>,
) -> Option<Doc> {
    let unit_return = return_type.is_none_or(is_unit_type);
    let mut parts = Vec::new();
    if !unit_return {
        parts.push(text("-> "));
        parts.push(type_expr_to_doc(return_type.expect("unit_return is false")));
    }
    if let Some(error_type) = error_type {
        if !unit_return {
            parts.push(line());
        }
        parts.push(text("! "));
        parts.push(type_expr_to_doc(error_type));
    }
    if parts.is_empty() {
        return None;
    }
    Some(group(concat(parts)))
}

/// The qualified head of an enum pattern (`Shape.Rect`), or the bare
/// variant when the path is empty.
pub(super) fn enum_prefix(type_path: &[String], variant: &str) -> String {
    if type_path.is_empty() {
        variant.to_string()
    } else {
        format!("{}.{}", type_path.join("."), variant)
    }
}

/// The comment-free pattern layout. [`Printer::pattern_to_doc`] falls
/// back to this when no comment sits inside the pattern.
pub(super) fn pattern_to_doc(pat: &Pattern) -> Doc {
    match pat {
        Pattern::Wildcard { .. } => text("_"),
        Pattern::Literal { value, .. } => literal_to_doc(value),
        Pattern::Binding { name, .. } => text(name.clone()),
        Pattern::EnumUnit {
            type_path, variant, ..
        } => text(enum_prefix(type_path, variant)),
        Pattern::EnumTuple {
            type_path,
            variant,
            elements,
            ..
        } => {
            let prefix = enum_prefix(type_path, variant);
            if elements.is_empty() {
                text(prefix)
            } else {
                let elems: Vec<Doc> = elements.iter().map(pattern_to_doc).collect();
                concat(vec![
                    text(prefix),
                    text("("),
                    intersperse(elems, text(", ")),
                    text(")"),
                ])
            }
        }
        Pattern::EnumStruct {
            type_path,
            variant,
            fields,
            ..
        } => struct_pattern_to_doc(&enum_prefix(type_path, variant), fields),
        Pattern::Struct {
            type_path, fields, ..
        } => struct_pattern_to_doc(&type_path.join("."), fields),
        Pattern::Constructor { name, elements, .. } => {
            if elements.is_empty() {
                text(name.clone())
            } else {
                let elems: Vec<Doc> = elements.iter().map(pattern_to_doc).collect();
                concat(vec![
                    text(name.clone()),
                    text("("),
                    intersperse(elems, text(", ")),
                    text(")"),
                ])
            }
        }
        Pattern::TypedBinding {
            name, type_expr, ..
        } => concat(vec![
            text(name.clone()),
            text(": "),
            type_expr_to_doc(type_expr),
        ]),
        Pattern::List { elements, .. } => {
            let elems: Vec<Doc> = elements.iter().map(pattern_to_doc).collect();
            concat(vec![text("["), intersperse(elems, text(", ")), text("]")])
        }
        Pattern::Tuple { elements, .. } => {
            let elems: Vec<Doc> = elements.iter().map(pattern_to_doc).collect();
            concat(vec![text("("), intersperse(elems, text(", ")), text(")")])
        }
        Pattern::Binary { segments, .. } => {
            if segments.is_empty() {
                text("<<>>")
            } else {
                // Segment values are full expressions, so they go
                // through the expression printer. This path only runs
                // for comment-free patterns, so an empty table renders
                // the same as the commented printer would.
                let mut p = Printer::pure();
                let seg_docs: Vec<Doc> = segments
                    .iter()
                    .map(|seg| p.binary_segment_to_doc(seg))
                    .collect();
                fill_bracket_list("<<", ">>", seg_docs)
            }
        }
        Pattern::Or { patterns, .. } => {
            let len = patterns.len();
            let mut items: Vec<Doc> = Vec::with_capacity(len);
            for (i, pat) in patterns.iter().enumerate() {
                if i < len - 1 {
                    items.push(concat(vec![pattern_to_doc(pat), text(" |")]));
                } else {
                    items.push(pattern_to_doc(pat));
                }
            }
            fill(items)
        }
    }
}

fn field_pattern_to_doc(fp: &FieldPattern) -> Doc {
    concat(vec![
        text(&fp.name),
        text(": "),
        pattern_to_doc(&fp.pattern),
    ])
}

/// Shared `Type{f1, f2, ...}` rendering for both enum-struct variant
/// patterns and plain struct patterns. `prefix` is the qualified head,
/// e.g. `"Shape.Rect"` or `"Point"`.
fn struct_pattern_to_doc(prefix: &str, fields: &[FieldPattern]) -> Doc {
    let field_docs: Vec<Doc> = fields.iter().map(field_pattern_to_doc).collect();
    group(concat(vec![
        text(prefix.to_string()),
        text("{"),
        indent(
            2,
            concat(vec![
                softline(),
                intersperse(field_docs, concat(vec![text(","), line()])),
            ]),
        ),
        softline(),
        text("}"),
    ]))
}

pub(super) fn literal_to_doc(lit: &Literal) -> Doc {
    match lit {
        Literal::Bool(true) => text("true"),
        Literal::Bool(false) => text("false"),
        Literal::Float(s) => text(s.clone()),
        Literal::Int(s) => text(s.clone()),
        Literal::String(s) => text(format!("\"{}\"", escape_string_literal(s))),
        Literal::Unit => text("()"),
    }
}

pub(super) fn closure_param_to_doc(cp: &ClosureParam) -> Doc {
    match cp {
        ClosureParam::Name {
            name, type_expr, ..
        } => {
            let mut parts = Vec::new();
            parts.push(text(name.clone()));
            if let Some(te) = type_expr {
                parts.push(text(": "));
                parts.push(type_expr_to_doc(te));
            }
            concat(parts)
        }
        ClosureParam::Wildcard { .. } => text("_"),
    }
}

/// True for the `keyword ... end` constructs, which always span lines.
pub(super) fn is_block_expr(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::If { .. }
            | ExprKind::Match { .. }
            | ExprKind::Cond { .. }
            | ExprKind::For { .. }
            | ExprKind::Loop { .. }
            | ExprKind::While { .. }
            | ExprKind::Closure { .. }
            | ExprKind::Receive { .. }
    )
}

/// Returns `true` if the expression is a closure whose single statement
/// can render on one line (e.g. `fn (x: Int) -> Int x * 2 end`). A block
/// or heredoc body always spans lines, so collapsing around it would
/// fuse the signature with the block header.
pub(super) fn is_inline_closure(expr: &Expr) -> bool {
    let ExprKind::Closure { body, .. } = &expr.kind else {
        return false;
    };
    matches!(&body[..], [stmt] if !stmt_renders_multiline(stmt))
}

/// Returns `true` if the value survives heredoc form. The renderer trims
/// trailing whitespace from every output line, so content with a space
/// before a line end round-trips through single-line form instead.
pub(super) fn heredoc_representable(parts: &[StringPart]) -> bool {
    parts.iter().enumerate().all(|(i, part)| match part {
        StringPart::Literal { value, .. } => {
            !value.contains(" \n") && (i + 1 != parts.len() || !value.ends_with(' '))
        }
        StringPart::Interpolation { .. } => true,
    })
}

/// Returns `true` if the expression is a multiline string that will
/// render in heredoc form (block-shaped, forcing hard line breaks).
pub(super) fn is_heredoc(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::String { multiline: true, parts } if heredoc_representable(parts))
}

/// True when the statement is, or assigns, a block expression.
pub(super) fn stmt_is_block(stmt: &Statement) -> bool {
    match stmt {
        Statement::Expr(expr) => is_block_expr(expr),
        Statement::Assignment { value, .. } => is_block_expr(value),
        Statement::CompoundAssign { value, .. } => is_block_expr(value),
        Statement::Return { value: Some(v), .. } => is_block_expr(v),
        _ => false,
    }
}

/// Returns `true` if the statement always renders across multiple lines
/// (a block construct or a heredoc), so it can never collapse inline.
pub(super) fn stmt_renders_multiline(stmt: &Statement) -> bool {
    let value = match stmt {
        Statement::Expr(expr) => expr,
        Statement::Assignment { value, .. }
        | Statement::CompoundAssign { value, .. }
        | Statement::Destructure { value, .. } => value,
        Statement::Return { value: Some(v), .. } => v,
        _ => return false,
    };
    is_block_expr(value) || is_heredoc(value)
}

/// Returns `true` if a match/cond arm body should be formatted across
/// multiple lines (more than one statement, or a single block expression).
pub(super) fn arm_is_multiline(body: &[Statement]) -> bool {
    if body.len() > 1 {
        return true;
    }
    if let [Statement::Expr(expr)] = body {
        return is_block_expr(expr);
    }
    false
}

/// Page width (80) minus a conservative minimum arm indentation. A
/// single-expression arm body whose `head -> body` estimate exceeds
/// this would width-wrap at render time, so we treat the whole arm as
/// multi-line up front and break every sibling consistently.
const ARM_INLINE_BUDGET: usize = 72;

/// Returns `true` if a single-expression arm body, laid out inline
/// after a `head -> ` of `head_len` columns, would overflow the page
/// and width-wrap. Multi-statement and block-expression bodies are
/// already caught by [`arm_is_multiline`], so they return `false` here.
pub(super) fn arm_body_overflows(head_len: usize, body: &[Statement]) -> bool {
    let [Statement::Expr(expr)] = body else {
        return false;
    };
    if is_block_expr(expr) {
        return false;
    }
    head_len + " -> ".len() + expr_text_len(expr) > ARM_INLINE_BUDGET
}

pub(super) fn pattern_is_multiline(pattern: &Pattern) -> bool {
    if let Pattern::Or { patterns, .. } = pattern {
        let estimated_width: usize = patterns.iter().map(pattern_text_len).sum::<usize>()
            + (patterns.len().saturating_sub(1)) * 3;
        return estimated_width > 60;
    }
    false
}

fn pattern_text_len(pattern: &Pattern) -> usize {
    match pattern {
        Pattern::Literal { value, .. } => literal_text_len(value),
        Pattern::Binding { name, .. } => name.len(),
        Pattern::Wildcard { .. } => 1,
        Pattern::Or { patterns, .. } => {
            patterns.iter().map(pattern_text_len).sum::<usize>()
                + (patterns.len().saturating_sub(1)) * 3
        }
        _ => 10,
    }
}

/// Exact single-line rendered width of a pattern. Patterns are pure
/// `Doc`s (no comment cursor), so rendering one flat measures its width
/// without the per-kind estimation drift of `pattern_text_len`.
pub(super) fn pattern_rendered_len(pattern: &Pattern) -> usize {
    render(&pattern_to_doc(pattern), u32::MAX).chars().count()
}

/// Estimates whether a chained `or` or `and` expression would exceed the page width.
pub(super) fn expr_or_is_multiline(expr: &Expr) -> bool {
    if let ExprKind::Binary {
        op: op @ (BinOp::Or | BinOp::And),
        ..
    } = &expr.kind
    {
        let operands = binop_operands(expr, op);
        if operands.len() <= 1 {
            return false;
        }
        let sep_len = binop_str(op).len() + 2;
        let estimated_width: usize = operands.iter().map(|e| expr_text_len(e)).sum::<usize>()
            + (operands.len().saturating_sub(1)) * sep_len;
        return estimated_width > 60;
    }
    false
}

/// The operands of a same-operator binary chain (`a + b + c`), left to
/// right. A subtree under a different operator is one operand.
pub(super) fn binop_operands<'a>(expr: &'a Expr, op: &BinOp) -> Vec<&'a Expr> {
    fn collect<'a>(expr: &'a Expr, target_op: &BinOp, out: &mut Vec<&'a Expr>) {
        if let ExprKind::Binary { op, left, right } = &expr.kind
            && std::mem::discriminant(op) == std::mem::discriminant(target_op)
        {
            collect(left, target_op, out);
            collect(right, target_op, out);
            return;
        }
        out.push(expr);
    }
    let mut out = Vec::new();
    collect(expr, op, &mut out);
    out
}

/// A method chain split into its root and the `.method(args)` links,
/// root-first. A plain call has one link, and a non-call has none.
pub(super) fn chain_links(expr: &Expr) -> (&Expr, Vec<&Expr>) {
    let mut links = Vec::new();
    let mut current = expr;
    while let ExprKind::MethodCall { receiver, .. } = &current.kind {
        links.push(current);
        current = receiver;
    }
    links.reverse();
    (current, links)
}

fn literal_text_len(lit: &Literal) -> usize {
    match lit {
        Literal::Bool(true) => 4,
        Literal::Bool(false) => 5,
        Literal::Float(f) => f.len(),
        Literal::Int(n) => n.len(),
        Literal::String(s) => s.len() + 2,
        Literal::Unit => 2,
    }
}

pub(super) fn expr_text_len(expr: &Expr) -> usize {
    match &expr.kind {
        ExprKind::Literal { value } => literal_text_len(value),
        ExprKind::Ident { name, .. } => name.len(),
        ExprKind::Self_ { .. } => 4,
        ExprKind::Binary { op, left, right } => {
            expr_text_len(left) + expr_text_len(right) + binop_str(op).len() + 2
        }
        ExprKind::Unary { operand, .. } => expr_text_len(operand) + 4,
        ExprKind::Call { callee, args, .. } => expr_text_len(callee) + call_args_text_len(args) + 2,
        ExprKind::MethodCall {
            receiver,
            method,
            args,
            ..
        } => expr_text_len(receiver) + 1 + method.len() + call_args_text_len(args) + 2,
        ExprKind::FieldAccess { receiver, field } => expr_text_len(receiver) + 1 + field.len(),
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => expr_text_len(condition) + expr_text_len(then_expr) + expr_text_len(else_expr) + 6,
        ExprKind::Group { expr } => expr_text_len(expr) + 2,
        ExprKind::String { parts, .. } => {
            2 + parts
                .iter()
                .map(|part| match part {
                    StringPart::Literal { value, .. } => value.len(),
                    StringPart::Interpolation { expr, .. } => expr_text_len(expr) + 3,
                })
                .sum::<usize>()
        }
        ExprKind::List { elements } => {
            2 + elements.iter().map(expr_text_len).sum::<usize>()
                + elements.len().saturating_sub(1) * 2
        }
        ExprKind::EnumConstruction {
            type_path,
            variant,
            data,
        } => {
            let head = path_text_len(type_path) + 1 + variant.len();
            match data {
                EnumConstructionData::Unit => head,
                EnumConstructionData::Tuple(elements) => {
                    head + 2
                        + elements.iter().map(expr_text_len).sum::<usize>()
                        + elements.len().saturating_sub(1) * 2
                }
                EnumConstructionData::Struct(fields) => head + struct_fields_text_len(fields),
            }
        }
        ExprKind::StructConstruction { type_path, fields } => {
            path_text_len(type_path) + struct_fields_text_len(fields)
        }
        _ => 10,
    }
}

/// Estimates the rendered width of a call/method-call argument list,
/// excluding the surrounding parentheses (`+ 2` is the caller's job).
fn call_args_text_len(args: &[Arg]) -> usize {
    args.iter()
        .map(|a| a.name.as_ref().map_or(0, |n| n.len() + 2) + expr_text_len(&a.value))
        .sum::<usize>()
        + args.len().saturating_sub(1) * 2
}

/// Estimates the rendered width of a `{field: value, ...}` body,
/// including the braces.
fn struct_fields_text_len(fields: &[FieldInit]) -> usize {
    2 + fields
        .iter()
        .map(|f| f.name.len() + 2 + expr_text_len(&f.value))
        .sum::<usize>()
        + fields.len().saturating_sub(1) * 2
}

/// Estimates the rendered width of a dotted path (`Pkg.Type`).
fn path_text_len(path: &[String]) -> usize {
    path.iter().map(|s| s.len()).sum::<usize>() + path.len().saturating_sub(1)
}

pub(super) fn binop_str(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::And => "and",
        BinOp::Concat => "<>",
        BinOp::Div => "/",
        BinOp::Eq => "==",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Mod => "%",
        BinOp::Mul => "*",
        BinOp::NotEq => "!=",
        BinOp::Or => "or",
        BinOp::Sub => "-",
    }
}

fn is_unit_type(ty: &TypeExpr) -> bool {
    matches!(ty, TypeExpr::Unit { .. })
}

/// Returns `true` if the rendered signature wraps onto more than one line
/// at the given starting indentation. Rendering at `width - indent_cols`
/// from column 0 reproduces the printer's break decisions exactly (the
/// signature is alone on its opening line), so this stays in lockstep with
/// the layout rather than re-estimating widths by hand.
pub(super) fn signature_wraps(signature: &Doc, indent_cols: u32) -> bool {
    let available = DEFAULT_WIDTH.saturating_sub(indent_cols);
    render(signature, available).contains('\n')
}

/// Returns the source span of a statement. `Statement::Expr` has no
/// wrapper span, so its expression's span stands in.
pub(super) fn stmt_span(stmt: &Statement) -> Span {
    match stmt {
        Statement::Expr(expr) => expr.span,
        Statement::Assignment { span, .. }
        | Statement::CompoundAssign { span, .. }
        | Statement::Destructure { span, .. }
        | Statement::Return { span, .. }
        | Statement::Break { span, .. } => *span,
    }
}

/// The span of a map entry, from the key's start to the value's end.
/// Shared by the attachment walk and the printer so both sides key the
/// entry identically.
pub(super) fn map_entry_span(key: &Expr, value: &Expr) -> Span {
    Span::new(key.span.start, value.span.end, key.span.file)
}

/// Last source line of a function signature: the header's own line, or
/// the end of the last parameter / return type / error type when the
/// signature wraps. The attach pass extends this to the closing `)`
/// line, located through the token stream, when the paren sits alone
/// on a later line.
pub(super) fn signature_end_line(
    header_start: u32,
    params: &[Param],
    return_type: Option<&TypeExpr>,
    error_type: Option<&TypeExpr>,
) -> u32 {
    let mut line = header_start;
    for param in params {
        line = line.max(param_span(param).end.line);
    }
    for type_expr in [return_type, error_type].into_iter().flatten() {
        line = line.max(type_expr_span(type_expr).end.line);
    }
    line
}

pub(super) fn param_span(param: &Param) -> &Span {
    match param {
        Param::Regular { span, .. } | Param::Self_ { span, .. } => span,
    }
}

/// Escapes special characters in a single-line string literal so the
/// formatter's output round-trips through the parser.
pub(super) fn escape_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '#' if chars.peek() == Some(&'{') => out.push_str("\\#"),
            _ => out.push(c),
        }
    }
    out
}

/// Escapes special characters in a multiline (`"""` heredoc) string body.
///
/// Unlike [`escape_string_literal`], `\n` stays a raw newline (the whole
/// point of multiline literals) and lone quotes stay raw. Only every
/// third quote of a consecutive run is escaped, since three raw quotes
/// would lex as the closing delimiter. `\\`, `\r`, `\t`, and `#{` are
/// escaped so the formatted output re-parses to the same value.
pub(super) fn escape_multiline_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut quote_run = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            quote_run += 1;
            if quote_run == 3 {
                out.push_str("\\\"");
                quote_run = 0;
            } else {
                out.push('"');
            }
            continue;
        }
        quote_run = 0;
        match c {
            '\\' => out.push_str("\\\\"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '#' if chars.peek() == Some(&'{') => out.push_str("\\#"),
            _ => out.push(c),
        }
    }
    out
}
