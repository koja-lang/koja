//! Pure utility functions for the formatter.
//!
//! Everything here is stateless -- no comment cursor, no `&mut self`. These
//! convert AST fragments (types, patterns, literals, imports, annotations)
//! into `Doc` nodes, and provide span / text-length helpers used by the
//! printer and expression modules.

use crate::doc::*;
use expo_ast::ast::*;
use expo_ast::span::Span;

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
        Item::Constant(c) => &c.span,
        Item::Enum(e) => &e.span,
        Item::Function(f) => &f.span,
        Item::Impl(i) => &i.span,
        Item::Import(i) => &i.span,
        Item::Protocol(p) => &p.span,
        Item::Shared(s) => &s.span,
        Item::Struct(s) => &s.span,
        Item::TypeAlias(t) => &t.span,
    }
}

/// Produces a stable sort key for an import so that the formatter can
/// alphabetize import groups deterministically.
pub(super) fn import_sort_key(imp: &Import) -> String {
    let base = imp.path.join(".");
    match &imp.target {
        ImportTarget::Module => base,
        ImportTarget::Item(name) => format!("{}.{}", base, name),
        ImportTarget::Group(_) => base,
        ImportTarget::Wildcard => format!("{}.*", base),
    }
}

/// Formats an `import` statement.
pub(super) fn import_to_doc(imp: &Import) -> Doc {
    let path_str = imp.path.join(".");
    match &imp.target {
        ImportTarget::Module => text(format!("import {}", path_str)),
        ImportTarget::Item(name) => text(format!("import {}.{}", path_str, name)),
        ImportTarget::Group(names) => {
            let items: Vec<Doc> = names.iter().map(|n| text(n.clone())).collect();
            concat(vec![
                text(format!("import {}.", path_str)),
                fill_bracket_list("{", "}", items),
            ])
        }
        ImportTarget::Wildcard => text(format!("import {}.*", path_str)),
    }
}

/// Formats a `type` alias declaration (`type Name = TypeExpr`).
pub(super) fn type_alias_to_doc(t: &TypeAlias) -> Doc {
    concat(vec![
        text("type "),
        text(&t.name),
        text(" = "),
        type_expr_to_doc(&t.type_expr),
    ])
}

/// Formats a `shared` declaration (`shared Name: TypeExpr`).
pub(super) fn shared_to_doc(s: &SharedDecl) -> Doc {
    concat(vec![
        text("shared "),
        text(&s.name),
        text(": "),
        type_expr_to_doc(&s.type_expr),
    ])
}

/// Formats an annotation (`@doc`, `@spec`, etc.).
pub(super) fn annotation_to_doc(ann: &Annotation) -> Doc {
    match &ann.value {
        Some(AnnotationValue::String(val)) => {
            if val.contains('\n') {
                concat(vec![
                    text(format!("@{} \"\"\"", ann.name)),
                    hardline(),
                    text(val.trim()),
                    hardline(),
                    text("\"\"\""),
                ])
            } else {
                text(format!("@{} \"{}\"", ann.name, val))
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
        TypeExpr::Tuple { elements, .. } => {
            let elems: Vec<Doc> = elements.iter().map(type_expr_to_doc).collect();
            concat(vec![text("("), intersperse(elems, text(", ")), text(")")])
        }
        TypeExpr::Unit { .. } => text("()"),
        TypeExpr::Self_ { .. } => text("Self"),
        TypeExpr::Function {
            params,
            param_modes,
            return_type,
            ..
        } => {
            let params_doc: Vec<Doc> = params
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let is_move = param_modes.get(i).is_some_and(|m| *m == PassMode::Move);
                    if is_move {
                        concat(vec![text("move "), type_expr_to_doc(p)])
                    } else {
                        type_expr_to_doc(p)
                    }
                })
                .collect();
            concat(vec![
                text("fn("),
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
            let field_docs: Vec<Doc> = fields.iter().map(field_pattern_to_doc).collect();
            group(concat(vec![
                text(prefix),
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
        Pattern::Tuple { elements, .. } => {
            let elems: Vec<Doc> = elements.iter().map(pattern_to_doc).collect();
            concat(vec![text("("), intersperse(elems, text(", ")), text(")")])
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
    }
}

/// Formats a single field pattern inside a struct destructure.
pub(super) fn field_pattern_to_doc(fp: &FieldPattern) -> Doc {
    match &fp.pattern {
        Some(pat) => concat(vec![text(&fp.name), text(": "), pattern_to_doc(pat)]),
        None => text(&fp.name),
    }
}

/// Formats a literal value.
pub(super) fn literal_to_doc(lit: &Literal) -> Doc {
    match lit {
        Literal::Bool(true) => text("true"),
        Literal::Bool(false) => text("false"),
        Literal::Float(s) => text(s.clone()),
        Literal::Int(s) => text(s.clone()),
        Literal::Unit => text("()"),
    }
}

/// Formats a closure parameter.
pub(super) fn closure_param_to_doc(cp: &ClosureParam) -> Doc {
    match cp {
        ClosureParam::Name {
            name, type_expr, ..
        } => {
            if let Some(te) = type_expr {
                concat(vec![text(name.clone()), text(": "), type_expr_to_doc(te)])
            } else {
                text(name.clone())
            }
        }
        ClosureParam::Destructured { names, .. } => concat(vec![
            text("("),
            intersperse(names.iter().map(|n| text(n.clone())).collect(), text(", ")),
            text(")"),
        ]),
        ClosureParam::Wildcard { .. } => text("_"),
    }
}

/// Returns `true` if the expression is a multi-line block construct
/// (if, match, cond, for, loop, unless, while, closure, receive, arena).
pub(super) fn is_block_expr(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::If { .. }
            | Expr::Match { .. }
            | Expr::Cond { .. }
            | Expr::For { .. }
            | Expr::Loop { .. }
            | Expr::Unless { .. }
            | Expr::While { .. }
            | Expr::Closure { .. }
            | Expr::Receive { .. }
            | Expr::Arena { .. }
    )
}

/// Returns `true` if the statement is an assignment whose value is a
/// block expression (match, cond, if, receive, for, loop, while, etc.).
pub(super) fn is_block_assignment(stmt: &Statement) -> bool {
    matches!(stmt, Statement::Assignment { value, .. } if is_block_expr(value))
}

/// Returns `true` if the expression contains multi-line block constructs
/// that warrant breaking after `=`.
pub(super) fn expr_contains_block(expr: &Expr) -> bool {
    if is_block_expr(expr) {
        return true;
    }
    match expr {
        Expr::Call { args, .. } => args.iter().any(|a| expr_contains_block(&a.value)),
        Expr::MethodCall { receiver, args, .. } => {
            expr_contains_block(receiver) || args.iter().any(|a| expr_contains_block(&a.value))
        }
        Expr::Binary { right, .. } => expr_contains_block(right),
        Expr::Await { expr, .. } => expr_contains_block(expr),
        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
            ..
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
        len += f.type_params.join(", ").len();
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
        Param::Self_ { mode, .. } => {
            if *mode == PassMode::Move {
                9
            } else {
                4
            }
        }
        Param::Regular {
            mode,
            name,
            type_expr,
            default,
            ..
        } => {
            let mut n = 0;
            if *mode == PassMode::Move {
                n += 5;
            }
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
        TypeExpr::Tuple { elements, .. } => {
            let inner: usize = elements.iter().map(type_expr_text_len).sum::<usize>()
                + elements.len().saturating_sub(1) * 2;
            1 + inner + 1
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
            3 + params_len + 5 + type_expr_text_len(return_type)
        }
        TypeExpr::Union { types, .. } => {
            let inner: usize = types.iter().map(type_expr_text_len).sum::<usize>();
            inner + types.len().saturating_sub(1) * 3 // " | " between each
        }
    }
}

/// Extracts the source span from any expression node.
fn expr_span(expr: &Expr) -> &Span {
    use expo_ast::ast::Expr::*;
    match expr {
        Arena { span, .. }
        | Await { span, .. }
        | Binary { span, .. }
        | Call { span, .. }
        | Closure { span, .. }
        | Cond { span, .. }
        | EnumConstruction { span, .. }
        | FieldAccess { span, .. }
        | For { span, .. }
        | Group { span, .. }
        | Ident { span, .. }
        | If { span, .. }
        | List { span, .. }
        | Map { span, .. }
        | Literal { span, .. }
        | Loop { span, .. }
        | Match { span, .. }
        | MethodCall { span, .. }
        | Receive { span, .. }
        | Self_ { span, .. }
        | ShortClosure { span, .. }
        | Spawn { span, .. }
        | String { span, .. }
        | StructConstruction { span, .. }
        | Ternary { span, .. }
        | Tuple { span, .. }
        | Unary { span, .. }
        | Unless { span, .. }
        | While { span, .. } => span,
    }
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
            '\t' => out.push_str("\\t"),
            '#' if chars.peek() == Some(&'{') => out.push_str("\\#"),
            _ => out.push(c),
        }
    }
    out
}
