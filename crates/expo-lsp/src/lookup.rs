use expo_ast::ast::*;
use expo_ast::span::Span;
use expo_typecheck::context::TypeContext;

#[derive(Debug)]
pub enum SymbolInfo {
    Function { name: String },
    Struct { name: String },
    Enum { name: String },
    Module { path: Vec<String> },
    ModuleFunction { module: String, name: String },
    Variable { name: String },
}

pub fn find_symbol_at(
    module: &Module,
    line: u32,
    col: u32,
    ctx: &TypeContext,
) -> Option<SymbolInfo> {
    for item in &module.items {
        match item {
            Item::Function(f) => {
                if !span_contains(&f.span, line, col) {
                    continue;
                }
                if let Some(info) = find_in_ident_at_name(&f.name, &f.span, line, col, ctx) {
                    return Some(info);
                }
                for stmt in &f.body {
                    if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
            Item::Impl(imp) => {
                for member in &imp.members {
                    if let ImplMember::Function(f) = member {
                        if !span_contains(&f.span, line, col) {
                            continue;
                        }
                        for stmt in &f.body {
                            if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                                return Some(info);
                            }
                        }
                    }
                }
            }
            Item::Struct(s) => {
                if span_contains_name(&s.name, &s.span, line, col) {
                    return Some(SymbolInfo::Struct {
                        name: s.name.clone(),
                    });
                }
            }
            Item::Enum(e) => {
                if span_contains_name(&e.name, &e.span, line, col) {
                    return Some(SymbolInfo::Enum {
                        name: e.name.clone(),
                    });
                }
            }
            Item::Import(imp) => {
                if span_contains(&imp.span, line, col) {
                    return Some(SymbolInfo::Module {
                        path: imp.path.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    None
}

pub fn find_doc_for(module: &Module, name: &str) -> Option<String> {
    for item in &module.items {
        match item {
            Item::Function(f) if f.name == name => {
                return annotation_doc(&f.annotation);
            }
            Item::Struct(s) if s.name == name => {
                return annotation_doc(&s.annotation);
            }
            Item::Enum(e) if e.name == name => {
                return annotation_doc(&e.annotation);
            }
            Item::Impl(imp) => {
                for member in &imp.members {
                    if let ImplMember::Function(f) = member
                        && f.name == name
                    {
                        return annotation_doc(&f.annotation);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn annotation_doc(annotation: &Option<Annotation>) -> Option<String> {
    annotation
        .as_ref()
        .filter(|a| a.name == "doc")
        .and_then(|a| a.value.clone())
}

fn span_contains(span: &Span, line: u32, col: u32) -> bool {
    if line < span.start.line || line > span.end.line {
        return false;
    }
    if line == span.start.line && col < span.start.column {
        return false;
    }
    if line == span.end.line && col > span.end.column {
        return false;
    }
    true
}

fn span_contains_name(_name: &str, span: &Span, line: u32, col: u32) -> bool {
    span.start.line == line && col >= span.start.column && col <= span.end.column
}

fn find_in_ident_at_name(
    name: &str,
    span: &Span,
    line: u32,
    col: u32,
    ctx: &TypeContext,
) -> Option<SymbolInfo> {
    if span.start.line != line {
        return None;
    }
    let name_start = span.start.column;
    let fn_keyword_len = if name_start >= 4 { 3 } else { 0 };
    let ident_start = name_start + fn_keyword_len;
    let ident_end = ident_start + name.len() as u32;

    if col >= ident_start && col <= ident_end {
        return classify_name(name, ctx);
    }
    None
}

fn find_in_statement(
    stmt: &Statement,
    line: u32,
    col: u32,
    ctx: &TypeContext,
) -> Option<SymbolInfo> {
    match stmt {
        Statement::Expr(expr) => find_in_expr(expr, line, col, ctx),
        Statement::Assignment { value, .. } => find_in_expr(value, line, col, ctx),
        Statement::CompoundAssign { value, .. } => find_in_expr(value, line, col, ctx),
        Statement::Return {
            value: Some(expr), ..
        } => find_in_expr(expr, line, col, ctx),
        _ => None,
    }
}

fn find_in_expr(expr: &Expr, line: u32, col: u32, ctx: &TypeContext) -> Option<SymbolInfo> {
    match expr {
        Expr::Ident { name, span } => {
            if span_contains(span, line, col) {
                return classify_name(name, ctx);
            }
        }
        Expr::Call {
            callee, args, span, ..
        } => {
            if span_contains(span, line, col) {
                if let Some(info) = find_in_expr(callee, line, col, ctx) {
                    return Some(info);
                }
                for arg in args {
                    if let Some(info) = find_in_expr(&arg.value, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        Expr::MethodCall {
            receiver,
            method,
            args,
            span,
            ..
        } => {
            if span_contains(span, line, col) {
                if let Some(info) = find_in_expr(receiver, line, col, ctx) {
                    return Some(info);
                }
                if let Expr::Ident {
                    name: mod_name,
                    span: recv_span,
                } = receiver.as_ref()
                {
                    let method_start = recv_span.end.column + 2;
                    let method_end = method_start + method.len() as u32;
                    if line == recv_span.end.line
                        && col >= method_start
                        && col <= method_end
                        && ctx.imported_modules.contains_key(mod_name)
                    {
                        return Some(SymbolInfo::ModuleFunction {
                            module: mod_name.clone(),
                            name: method.clone(),
                        });
                    }
                }
                for arg in args {
                    if let Some(info) = find_in_expr(&arg.value, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        Expr::FieldAccess { receiver, span, .. } => {
            if span_contains(span, line, col)
                && let Some(info) = find_in_expr(receiver, line, col, ctx)
            {
                return Some(info);
            }
        }
        Expr::Binary {
            left, right, span, ..
        } => {
            if span_contains(span, line, col) {
                if let Some(info) = find_in_expr(left, line, col, ctx) {
                    return Some(info);
                }
                if let Some(info) = find_in_expr(right, line, col, ctx) {
                    return Some(info);
                }
            }
        }
        Expr::If {
            condition,
            then_body,
            else_body,
            span,
        } => {
            if span_contains(span, line, col) {
                if let Some(info) = find_in_expr(condition, line, col, ctx) {
                    return Some(info);
                }
                for stmt in then_body {
                    if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                        return Some(info);
                    }
                }
                if let Some(else_stmts) = else_body {
                    for stmt in else_stmts {
                        if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                            return Some(info);
                        }
                    }
                }
            }
        }
        Expr::Match {
            subject,
            arms,
            span,
        } => {
            if span_contains(span, line, col) {
                if let Some(info) = find_in_expr(subject, line, col, ctx) {
                    return Some(info);
                }
                for arm in arms {
                    for stmt in &arm.body {
                        if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                            return Some(info);
                        }
                    }
                }
            }
        }
        Expr::Cond {
            arms,
            else_body,
            span,
        } => {
            if span_contains(span, line, col) {
                for arm in arms {
                    if let Some(info) = find_in_expr(&arm.condition, line, col, ctx) {
                        return Some(info);
                    }
                    for stmt in &arm.body {
                        if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                            return Some(info);
                        }
                    }
                }
                if let Some(body) = else_body {
                    for stmt in body {
                        if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                            return Some(info);
                        }
                    }
                }
            }
        }
        Expr::Group { expr, span } => {
            if span_contains(span, line, col) {
                return find_in_expr(expr, line, col, ctx);
            }
        }
        Expr::StructConstruction {
            type_path, span, ..
        } => {
            if span_contains(span, line, col) {
                let name = type_path.last()?;
                return classify_name(name, ctx);
            }
        }
        Expr::EnumConstruction {
            type_path, span, ..
        } => {
            if span_contains(span, line, col) {
                let name = type_path.first()?;
                return classify_name(name, ctx);
            }
        }
        Expr::While {
            condition,
            body,
            span,
        } => {
            if span_contains(span, line, col) {
                if let Some(info) = find_in_expr(condition, line, col, ctx) {
                    return Some(info);
                }
                for stmt in body {
                    if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        Expr::Loop { body, span } => {
            if span_contains(span, line, col) {
                for stmt in body {
                    if let Some(info) = find_in_statement(stmt, line, col, ctx) {
                        return Some(info);
                    }
                }
            }
        }
        _ => {}
    }
    None
}

fn classify_name(name: &str, ctx: &TypeContext) -> Option<SymbolInfo> {
    if ctx.functions.contains_key(name) {
        Some(SymbolInfo::Function {
            name: name.to_string(),
        })
    } else if ctx.structs.contains_key(name) {
        Some(SymbolInfo::Struct {
            name: name.to_string(),
        })
    } else if ctx.enums.contains_key(name) {
        Some(SymbolInfo::Enum {
            name: name.to_string(),
        })
    } else if ctx.imported_modules.contains_key(name) {
        Some(SymbolInfo::Module {
            path: vec![name.to_string()],
        })
    } else {
        Some(SymbolInfo::Variable {
            name: name.to_string(),
        })
    }
}
