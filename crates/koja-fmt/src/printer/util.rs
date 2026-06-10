//! Pure utility functions for the formatter.
//!
//! Everything here is stateless -- no comment cursor, no `&mut self`. These
//! convert AST fragments (types, patterns, literals, imports, annotations)
//! into `Doc` nodes, and provide span / text-length helpers used by the
//! printer and expression modules.

use crate::doc::*;
use koja_ast::ast::*;
use koja_ast::span::Span;

/// Formats a `TypeParam` as a string, including bounds if present.
/// E.g. `T`, `T: Debug`, `T: Debug & Hash`.
pub fn format_type_param(tp: &TypeParam) -> String {
    if tp.bounds.is_empty() {
        tp.name.clone()
    } else {
        format!("{}: {}", tp.name, tp.bounds.join(" & "))
    }
}

/// Formats a list of `TypeParam`s as a comma-separated string.
pub fn format_type_params(tps: &[TypeParam]) -> String {
    tps.iter()
        .map(format_type_param)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Formats a comma-separated list of items using fill layout inside brackets.
///
/// Items are packed left-to-right on each line. A trailing comma is added
/// to all items except the last. The result is wrapped in a group so the
/// whole list can collapse to a single line when it fits.
pub(super) fn fill_bracket_list(open: &str, close: &str, items: Vec<Doc>) -> Doc {
    let last = items.len() - 1;
    let items: Vec<Doc> = items
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
    let fill_items: Vec<Doc> = items
        .into_iter()
        .enumerate()
        .map(|(i, d)| if i > 0 { concat(vec![text(" "), d]) } else { d })
        .collect();
    group(concat(vec![
        text(open),
        indent(2, concat(vec![softline(), fill(fill_items)])),
        softline(),
        text(close),
    ]))
}

/// Formats a struct-like body: `prefix{ field, field, ... }` with
/// trailing-comma layout that breaks across lines when needed.
pub(super) fn struct_body(prefix: Doc, field_docs: Vec<Doc>) -> Doc {
    group(concat(vec![
        prefix,
        text("{"),
        indent(
            2,
            concat(vec![
                softline(),
                intersperse(field_docs, concat(vec![text(","), line()])),
                trailing_comma(),
            ]),
        ),
        softline(),
        text("}"),
    ]))
}

/// Returns the source span for any top-level `Item`.
pub(super) fn item_span(item: &Item) -> &Span {
    match item {
        Item::Alias(a) => &a.span,
        Item::Constant(c) => &c.span,
        Item::Enum(e) => &e.span,
        Item::Extend(e) => &e.span,
        Item::Function(f) => &f.span,
        Item::Impl(i) => &i.span,
        Item::Protocol(p) => &p.span,
        Item::Struct(s) => &s.span,
        Item::TypeAlias(t) => &t.span,
    }
}

/// Formats an `alias` declaration (`alias pkg.Type` or `alias pkg.Type as Name`).
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

/// Formats a `type` alias declaration (`type Name = TypeExpr`).
pub(super) fn type_alias_to_doc(t: &TypeAlias) -> Doc {
    let mut parts = Vec::new();
    if let Some(doc) = annotations_to_doc(&t.annotations) {
        parts.push(doc);
        parts.push(hardline());
    }
    parts.push(text("type "));
    parts.push(text(&t.name));
    parts.push(text(" = "));
    parts.push(type_expr_to_doc(&t.type_expr));
    concat(parts)
}

/// Formats a list of annotations, preserving the stacked/inline layout.
/// Annotations on the same line are joined with a space; annotations on
/// separate lines are joined with hardlines.
pub(super) fn annotations_to_doc(annotations: &[Annotation]) -> Option<Doc> {
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

/// Formats a single annotation (`@doc`, `@spec`, etc.).
pub(super) fn annotation_to_doc(ann: &Annotation) -> Doc {
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

/// Formats a type expression (`Int32`, `List<T>`, `fn(A) -> B`, etc.).
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
        TypeExpr::Union { types, .. } => {
            let parts: Vec<Doc> = types.iter().map(type_expr_to_doc).collect();
            intersperse(parts, text(" | "))
        }
    }
}

/// Formats a pattern (used in match arms, for loops, destructuring).
pub(super) fn pattern_to_doc(pat: &Pattern) -> Doc {
    match pat {
        Pattern::Wildcard { .. } => text("_"),
        Pattern::Literal { value, .. } => literal_to_doc(value),
        Pattern::Binding { name, .. } => text(name.clone()),
        Pattern::EnumUnit {
            type_path, variant, ..
        } => {
            if type_path.is_empty() {
                text(variant.clone())
            } else {
                text(format!("{}.{}", type_path.join("."), variant))
            }
        }
        Pattern::EnumTuple {
            type_path,
            variant,
            elements,
            ..
        } => {
            let prefix = if type_path.is_empty() {
                variant.clone()
            } else {
                format!("{}.{}", type_path.join("."), variant)
            };
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
        } => {
            let prefix = if type_path.is_empty() {
                variant.clone()
            } else {
                format!("{}.{}", type_path.join("."), variant)
            };
            struct_pattern_to_doc(&prefix, fields)
        }
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
        Pattern::Binary { segments, .. } => {
            if segments.is_empty() {
                text("<<>>")
            } else {
                let seg_docs: Vec<Doc> = segments.iter().map(binary_segment_pat_to_doc).collect();
                concat(vec![
                    text("<<"),
                    intersperse(seg_docs, text(", ")),
                    text(">>"),
                ])
            }
        }
        Pattern::Or { patterns, .. } => {
            let len = patterns.len();
            let mut items: Vec<Doc> = Vec::with_capacity(len);
            for (i, pat) in patterns.iter().enumerate() {
                if i < len - 1 {
                    items.push(concat(vec![pattern_to_doc(pat), text(" | ")]));
                } else {
                    items.push(pattern_to_doc(pat));
                }
            }
            fill(items)
        }
    }
}

fn binary_segment_pat_to_doc(seg: &BinarySegment) -> Doc {
    let mut parts = vec![expr_value_to_doc(&seg.value)];
    if let Some(size) = &seg.size {
        parts.push(text("::"));
        parts.push(expr_value_to_doc(size));
        if seg.unit == BinaryUnit::Byte {
            parts.push(text(" byte"));
        }
        if let Some(s) = &seg.signedness {
            parts.push(text(match s {
                BinarySignedness::Signed => " signed",
                BinarySignedness::Unsigned => " unsigned",
            }));
        }
        if let Some(e) = &seg.endianness {
            parts.push(text(match e {
                BinaryEndianness::Big => " big",
                BinaryEndianness::Little => " little",
            }));
        }
    } else if let Some(ta) = &seg.type_ann {
        parts.push(text(": "));
        parts.push(type_expr_to_doc(ta));
    }
    concat(parts)
}

fn expr_value_to_doc(expr: &Expr) -> Doc {
    match &expr.kind {
        ExprKind::Ident { name, .. } => text(name.clone()),
        ExprKind::Literal { value } => literal_to_doc(value),
        ExprKind::String { parts, .. } => {
            let mut doc_parts = vec![text("\"")];
            for part in parts {
                match part {
                    StringPart::Literal { value, .. } => {
                        doc_parts.push(text(escape_string_literal(value)));
                    }
                    StringPart::Interpolation { expr, .. } => {
                        doc_parts.push(text("#{"));
                        doc_parts.push(expr_value_to_doc(expr));
                        doc_parts.push(text("}"));
                    }
                }
            }
            doc_parts.push(text("\""));
            concat(doc_parts)
        }
        _ => text("<expr>"),
    }
}

/// Formats a single field pattern inside a struct destructure.
pub(super) fn field_pattern_to_doc(fp: &FieldPattern) -> Doc {
    concat(vec![
        text(&fp.name),
        text(": "),
        pattern_to_doc(&fp.pattern),
    ])
}

/// Shared `Type{f1, f2, ...}` rendering for both enum-struct variant
/// patterns and plain struct patterns. `prefix` is the qualified head
/// (e.g. `"Shape.Rect"` or `"Point"`); `fields` are the listed field
/// patterns.
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

/// Formats a literal value.
pub(super) fn literal_to_doc(lit: &Literal) -> Doc {
    match lit {
        Literal::Bool(true) => text("true"),
        Literal::Bool(false) => text("false"),
        Literal::Float(s) => text(s.clone()),
        Literal::Int(s) => text(s.clone()),
        Literal::String(s) => {
            let escaped = s
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            text(format!("\"{}\"", escaped))
        }
        Literal::Unit => text("()"),
    }
}

/// Formats a closure parameter.
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

/// Returns `true` if the expression is a multi-line block construct
/// (if, match, cond, for, loop, unless, while, closure, receive).
pub(super) fn is_block_expr(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::If { .. }
            | ExprKind::Match { .. }
            | ExprKind::Cond { .. }
            | ExprKind::For { .. }
            | ExprKind::Loop { .. }
            | ExprKind::Unless { .. }
            | ExprKind::While { .. }
            | ExprKind::Closure { .. }
            | ExprKind::Receive { .. }
    )
}

/// Returns `true` if the expression is a single-statement closure —
/// one that the closure printer lays out inline when it fits (e.g.
/// `fn (x: Int) -> Int x * 2 end`). Assignments to such a closure stay
/// on one line rather than forcing a break after `=`.
pub(super) fn is_inline_closure(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Closure { body, .. } if body.len() == 1)
}

/// Returns `true` if the statement is or contains a block expression
/// at its top level (if, match, cond, while, for, loop, etc.).
pub(super) fn stmt_is_block(stmt: &Statement) -> bool {
    match stmt {
        Statement::Expr(expr) => is_block_expr(expr),
        Statement::Assignment { value, .. } => is_block_expr(value),
        Statement::CompoundAssign { value, .. } => is_block_expr(value),
        Statement::Return { value: Some(v), .. } => is_block_expr(v),
        _ => false,
    }
}

/// Returns `true` if the expression contains multi-line block constructs
/// that warrant breaking after `=`.
pub(super) fn expr_contains_block(expr: &Expr) -> bool {
    if is_block_expr(expr) {
        return true;
    }
    match &expr.kind {
        ExprKind::Call { args, .. } => args.iter().any(|a| expr_contains_block(&a.value)),
        ExprKind::MethodCall { receiver, args, .. } => {
            expr_contains_block(receiver) || args.iter().any(|a| expr_contains_block(&a.value))
        }
        ExprKind::Binary { right, .. } => expr_contains_block(right),
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_contains_block(condition)
                || expr_contains_block(then_expr)
                || expr_contains_block(else_expr)
        }
        _ => false,
    }
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
        Pattern::Literal { value, .. } => match value {
            Literal::Int(n) => n.to_string().len(),
            Literal::Float(f) => f.to_string().len(),
            Literal::String(s) => s.len() + 2,
            Literal::Bool(b) => {
                if *b {
                    4
                } else {
                    5
                }
            }
            Literal::Unit => 2,
        },
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
/// `Doc`s (no comment cursor), so we can render one to measure its flat
/// width and use it as the arm-head length when predicting whether a
/// single-expression body would overflow. This is exact for every
/// pattern shape, avoiding the per-kind estimation drift that
/// `pattern_text_len` (used only for the coarse or-pattern check)
/// carries.
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
        let mut operands = Vec::new();
        collect_binop_exprs(expr, op, &mut operands);
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

fn collect_binop_exprs<'a>(expr: &'a Expr, target_op: &BinOp, out: &mut Vec<&'a Expr>) {
    if let ExprKind::Binary { op, left, right } = &expr.kind
        && std::mem::discriminant(op) == std::mem::discriminant(target_op)
    {
        collect_binop_exprs(left, target_op, out);
        collect_binop_exprs(right, target_op, out);
        return;
    }
    out.push(expr);
}

pub(super) fn expr_text_len(expr: &Expr) -> usize {
    match &expr.kind {
        ExprKind::Literal { value } => match value {
            Literal::Int(n) => n.to_string().len(),
            Literal::Float(f) => f.to_string().len(),
            Literal::String(s) => s.len() + 2,
            Literal::Bool(b) => {
                if *b {
                    4
                } else {
                    5
                }
            }
            Literal::Unit => 2,
        },
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

/// Assembles a `keyword ... arms ... end` block.
///
/// Handles indented arm spacing (extra blank lines when `any_multiline`),
/// an optional suffix between the arms and `end` (e.g. `after` clause),
/// and the closing `end` keyword.
pub(super) fn arms_block(
    header: Doc,
    arm_docs: Vec<Doc>,
    any_multiline: bool,
    suffix: Vec<Doc>,
) -> Doc {
    let mut spaced = Vec::new();
    for (i, doc) in arm_docs.into_iter().enumerate() {
        spaced.push(hardline());
        if any_multiline && i > 0 {
            spaced.push(hardline());
        }
        spaced.push(doc);
    }
    let mut parts = vec![header];
    parts.push(indent(2, concat(spaced)));
    parts.extend(suffix);
    parts.push(hardline());
    parts.push(text("end"));
    concat(parts)
}

/// Returns the source-code string for a binary operator.
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

/// Returns `true` if the type expression is `()`.
pub(super) fn is_unit_type(ty: &TypeExpr) -> bool {
    matches!(ty, TypeExpr::Unit { .. })
}

/// Estimates whether a function signature will exceed 80 columns when
/// rendered on a single line, so the printer can pre-emptively break it.
pub(super) fn sig_will_break(f: &Function) -> bool {
    let mut len: usize = 0;
    if f.visibility == Visibility::Private {
        len += 5;
    }
    len += 3;
    len += f.name.len();
    if !f.type_params.is_empty() {
        len += 1;
        len += format_type_params(&f.type_params).len();
        len += 1;
    }
    len += 1;
    for (i, p) in f.params.iter().enumerate() {
        if i > 0 {
            len += 2;
        }
        len += param_text_len(p);
    }
    len += 1;
    if let Some(rt) = &f.return_type
        && !is_unit_type(rt)
    {
        len += 4;
        len += type_expr_text_len(rt);
    }
    len > 80
}

/// Estimates the rendered text length of a function parameter.
pub(super) fn param_text_len(p: &Param) -> usize {
    match p {
        Param::Self_ { .. } => 4,
        Param::Regular {
            name,
            type_expr,
            default,
            ..
        } => {
            let mut n = 0;
            n += name.len();
            n += 2;
            n += type_expr_text_len(type_expr);
            if let Some(_d) = default {
                n += 3;
                n += 20; // estimate
            }
            n
        }
    }
}

/// Estimates the rendered text length of a type expression.
pub(super) fn type_expr_text_len(ty: &TypeExpr) -> usize {
    match ty {
        TypeExpr::Named { path, .. } => {
            path.iter().map(|s| s.len()).sum::<usize>() + path.len().saturating_sub(1)
        }
        TypeExpr::Generic { path, args, .. } => {
            let path_len: usize =
                path.iter().map(|s| s.len()).sum::<usize>() + path.len().saturating_sub(1);
            let args_len: usize = args.iter().map(type_expr_text_len).sum::<usize>()
                + args.len().saturating_sub(1) * 2;
            path_len + 1 + args_len + 1
        }
        TypeExpr::Unit { .. } => 2,
        TypeExpr::Self_ { .. } => 4,
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            let params_len: usize = params.iter().map(type_expr_text_len).sum::<usize>()
                + params.len().saturating_sub(1) * 2;
            4 + params_len + 5 + type_expr_text_len(return_type)
        }
        TypeExpr::Union { types, .. } => {
            let inner: usize = types.iter().map(type_expr_text_len).sum::<usize>();
            inner + types.len().saturating_sub(1) * 3 // " | " between each
        }
    }
}

fn expr_span(expr: &Expr) -> &Span {
    &expr.span
}

/// Returns the first source line of a statement.
pub(super) fn stmt_start_line(stmt: &Statement) -> u32 {
    match stmt {
        Statement::Expr(expr) => expr_span(expr).start.line,
        Statement::Assignment { span, .. }
        | Statement::CompoundAssign { span, .. }
        | Statement::Return { span, .. }
        | Statement::Break { span, .. } => span.start.line,
    }
}

/// Returns the last source line of a statement.
pub(super) fn stmt_end_line(stmt: &Statement) -> u32 {
    match stmt {
        Statement::Expr(expr) => expr_span(expr).end.line,
        Statement::Assignment { span, .. }
        | Statement::CompoundAssign { span, .. }
        | Statement::Return { span, .. }
        | Statement::Break { span, .. } => span.end.line,
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

/// Escapes special characters in a multiline (`"""..."""`) string literal.
///
/// Unlike [`escape_string_literal`], we leave `\n` as a raw newline (the
/// whole point of multiline literals) and we don't escape `"` (a single
/// quote inside `"""..."""` is harmless; only the closing `"""` matters).
/// We do escape `\\`, `\r`, `\t`, and `#{` so the formatted output
/// re-parses to the same `String` value.
pub(super) fn escape_multiline_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
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
