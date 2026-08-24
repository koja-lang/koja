//! Normalize default parameters into exact-arity adapter functions.

use std::collections::HashSet;

use koja_ast::ast::{
    AnnotationKind, Arg, ClosureParam, Diagnostic, EnumConstructionData, Expr, ExprKind, Function,
    FunctionOrigin, ImplMember, Item, Param, Pattern, ProtocolMethod, Statement, StringPart,
};
use koja_ast::identifier::Resolution;

use crate::program::CheckedPackage;

pub(crate) fn normalize_packages(
    packages: &mut [CheckedPackage],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for package in packages {
        for file in &mut package.files {
            let mut items = Vec::new();
            for mut item in std::mem::take(&mut file.items) {
                match &mut item {
                    Item::Function(function) => {
                        let adapters = normalize_function(function, diagnostics);
                        items.push(item);
                        items.extend(adapters.into_iter().map(Item::Function));
                        continue;
                    }
                    Item::Builtin(decl) => normalize_functions(&mut decl.functions, diagnostics),
                    Item::Enum(decl) => normalize_functions(&mut decl.functions, diagnostics),
                    Item::Struct(decl) => normalize_functions(&mut decl.functions, diagnostics),
                    Item::Extend(block) => {
                        normalize_members(&mut block.members, false, diagnostics)
                    }
                    Item::Impl(block) => normalize_members(&mut block.members, true, diagnostics),
                    Item::Protocol(decl) => {
                        normalize_protocol_methods(&mut decl.methods, diagnostics)
                    }
                    _ => {}
                }
                items.push(item);
            }
            file.items = items;
        }
    }
}

fn normalize_functions(functions: &mut Vec<Function>, diagnostics: &mut Vec<Diagnostic>) {
    let mut normalized = Vec::new();
    for mut function in std::mem::take(functions) {
        let adapters = normalize_function(&mut function, diagnostics);
        normalized.push(function);
        normalized.extend(adapters);
    }
    *functions = normalized;
}

fn normalize_members(
    members: &mut Vec<ImplMember>,
    protocol_impl: bool,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut normalized = Vec::new();
    for mut member in std::mem::take(members) {
        let ImplMember::Function(function) = &mut member else {
            normalized.push(member);
            continue;
        };
        if protocol_impl && function.params.iter().any(param_has_default) {
            diagnostics.push(Diagnostic::error(
                format!(
                    "implementation of `{}` cannot declare default parameters. \
                     Defaults belong to the protocol",
                    function.name
                ),
                function.span,
            ));
            clear_defaults(&mut function.params);
            normalized.push(member);
            continue;
        }
        let adapters = normalize_function(function, diagnostics);
        normalized.push(member);
        normalized.extend(adapters.into_iter().map(ImplMember::Function));
    }
    *members = normalized;
}

fn normalize_protocol_methods(
    methods: &mut Vec<ProtocolMethod>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut normalized = Vec::new();
    for mut method in std::mem::take(methods) {
        validate_defaults(&method.params, diagnostics);
        let adapters = protocol_adapters(&method);
        clear_defaults(&mut method.params);
        normalized.push(method);
        normalized.extend(adapters);
    }
    *methods = normalized;
}

fn normalize_function(function: &mut Function, diagnostics: &mut Vec<Diagnostic>) -> Vec<Function> {
    validate_defaults(&function.params, diagnostics);
    let adapters = function_adapters(function);
    clear_defaults(&mut function.params);
    adapters
}

fn validate_defaults(params: &[Param], diagnostics: &mut Vec<Diagnostic>) {
    let forbidden: HashSet<&str> = params
        .iter()
        .filter_map(|param| match param {
            Param::Regular { name, .. } => Some(name.as_str()),
            Param::Self_ { .. } => None,
        })
        .collect();
    let has_self = params
        .iter()
        .any(|param| matches!(param, Param::Self_ { .. }));
    for param in params {
        let Param::Regular {
            default: Some(default),
            ..
        } = param
        else {
            continue;
        };
        check_default_expr(default, &forbidden, has_self, &mut Vec::new(), diagnostics);
    }
}

fn function_adapters(function: &Function) -> Vec<Function> {
    adapter_arities(&function.params)
        .map(|arity| {
            let span = function.span.as_synthetic();
            Function {
                annotations: adapter_annotations(&function.annotations),
                origin: FunctionOrigin::DefaultAdapter {
                    canonical_arity: function.params.len(),
                },
                visibility: function.visibility,
                name: function.name.clone(),
                type_params: function.type_params.clone(),
                params: explicit_params(&function.params, arity),
                return_type: function.return_type.clone(),
                error_type: function.error_type.clone(),
                body: Some(adapter_body(&function.name, &function.params, arity, span)),
                span,
            }
        })
        .collect()
}

fn protocol_adapters(method: &ProtocolMethod) -> Vec<ProtocolMethod> {
    adapter_arities(&method.params)
        .map(|arity| {
            let span = method.span.as_synthetic();
            ProtocolMethod {
                annotations: Vec::new(),
                origin: FunctionOrigin::DefaultAdapter {
                    canonical_arity: method.params.len(),
                },
                name: method.name.clone(),
                type_params: method.type_params.clone(),
                params: explicit_params(&method.params, arity),
                return_type: method.return_type.clone(),
                error_type: method.error_type.clone(),
                body: Some(adapter_body(&method.name, &method.params, arity, span)),
                span,
            }
        })
        .collect()
}

fn adapter_arities(params: &[Param]) -> impl Iterator<Item = usize> + '_ {
    let required = params
        .iter()
        .take_while(|param| !param_has_default(param))
        .count();
    required..params.len()
}

fn explicit_params(params: &[Param], arity: usize) -> Vec<Param> {
    let mut params = params[..arity].to_vec();
    clear_defaults(&mut params);
    params
}

fn adapter_body(
    name: &str,
    params: &[Param],
    arity: usize,
    span: koja_ast::span::Span,
) -> Vec<Statement> {
    let args = params
        .iter()
        .skip(usize::from(matches!(
            params.first(),
            Some(Param::Self_ { .. })
        )))
        .enumerate()
        .map(|(index, param)| {
            let absolute = index + usize::from(matches!(params.first(), Some(Param::Self_ { .. })));
            let mut value = if absolute < arity {
                let Param::Regular { name, .. } = param else {
                    unreachable!("self is only valid as the first parameter")
                };
                Expr::new(
                    ExprKind::Ident {
                        name: name.clone(),
                        resolution: Resolution::Unresolved,
                    },
                    span,
                )
            } else {
                let Param::Regular {
                    default: Some(default),
                    ..
                } = param
                else {
                    unreachable!("omitted adapter parameters have defaults")
                };
                default.clone()
            };
            value.span = value.span.as_synthetic();
            Arg {
                name: None,
                span: value.span,
                value,
            }
        })
        .collect();
    let kind = if matches!(params.first(), Some(Param::Self_ { .. })) {
        ExprKind::MethodCall {
            receiver: Box::new(Expr::new(ExprKind::Self_ { local_id: None }, span)),
            method: name.to_string(),
            args,
            target: Resolution::Unresolved,
            type_args: Vec::new(),
        }
    } else {
        ExprKind::Call {
            callee: Box::new(Expr::new(
                ExprKind::Ident {
                    name: name.to_string(),
                    resolution: Resolution::Unresolved,
                },
                span,
            )),
            args,
            type_args: Vec::new(),
        }
    };
    vec![Statement::Expr(Expr::new(kind, span))]
}

fn adapter_annotations(
    annotations: &[koja_ast::ast::Annotation],
) -> Vec<koja_ast::ast::Annotation> {
    annotations
        .iter()
        .filter(|annotation| matches!(annotation.kind(), AnnotationKind::Deprecated { .. }))
        .cloned()
        .collect()
}

fn clear_defaults(params: &mut [Param]) {
    for param in params {
        if let Param::Regular { default, .. } = param {
            *default = None;
        }
    }
}

fn param_has_default(param: &Param) -> bool {
    matches!(
        param,
        Param::Regular {
            default: Some(_),
            ..
        }
    )
}

fn check_default_expr(
    expr: &Expr,
    forbidden: &HashSet<&str>,
    has_self: bool,
    scopes: &mut Vec<HashSet<String>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let shadowed = |name: &str, scopes: &[HashSet<String>]| {
        scopes.iter().rev().any(|scope| scope.contains(name))
    };
    match &expr.kind {
        ExprKind::Ident { name, .. }
            if forbidden.contains(name.as_str()) && !shadowed(name, scopes) =>
        {
            diagnostics.push(Diagnostic::error(
                format!("default parameter value cannot reference parameter `{name}`"),
                expr.span,
            ));
        }
        ExprKind::Self_ { .. } if has_self => diagnostics.push(Diagnostic::error(
            "default parameter value cannot reference `self`".to_string(),
            expr.span,
        )),
        ExprKind::Closure { params, body, .. } => {
            scopes.push(closure_bindings(params));
            check_body(body, forbidden, has_self, scopes, diagnostics);
            scopes.pop();
        }
        ExprKind::ShortClosure { params, body } => {
            scopes.push(closure_bindings(params));
            check_default_expr(body, forbidden, has_self, scopes, diagnostics);
            scopes.pop();
        }
        ExprKind::Binary { left, right, .. } => for_exprs(
            [left.as_ref(), right.as_ref()],
            forbidden,
            has_self,
            scopes,
            diagnostics,
        ),
        ExprKind::BinaryLiteral { segments } => {
            for segment in segments {
                check_default_expr(&segment.value, forbidden, has_self, scopes, diagnostics);
                if let Some(size) = &segment.size {
                    check_default_expr(size, forbidden, has_self, scopes, diagnostics);
                }
            }
        }
        ExprKind::Call { callee, args, .. } => {
            check_default_expr(callee, forbidden, has_self, scopes, diagnostics);
            for arg in args {
                check_default_expr(&arg.value, forbidden, has_self, scopes, diagnostics);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            check_default_expr(receiver, forbidden, has_self, scopes, diagnostics);
            for arg in args {
                check_default_expr(&arg.value, forbidden, has_self, scopes, diagnostics);
            }
        }
        ExprKind::Cond { arms, else_body } => {
            for arm in arms {
                check_default_expr(&arm.condition, forbidden, has_self, scopes, diagnostics);
                check_body(&arm.body, forbidden, has_self, scopes, diagnostics);
            }
            if let Some(body) = else_body {
                check_body(body, forbidden, has_self, scopes, diagnostics);
            }
        }
        ExprKind::EnumConstruction { data, .. } => match data {
            EnumConstructionData::Struct(fields) => {
                for field in fields {
                    check_default_expr(&field.value, forbidden, has_self, scopes, diagnostics);
                }
            }
            EnumConstructionData::Tuple(elements) => {
                for element in elements {
                    check_default_expr(element, forbidden, has_self, scopes, diagnostics);
                }
            }
            EnumConstructionData::Unit => {}
        },
        ExprKind::Fail { value }
        | ExprKind::Try { expr: value }
        | ExprKind::Unary { operand: value, .. }
        | ExprKind::Group { expr: value }
        | ExprKind::Spawn { expr: value }
        | ExprKind::FieldAccess {
            receiver: value, ..
        } => check_default_expr(value, forbidden, has_self, scopes, diagnostics),
        ExprKind::For {
            pattern,
            iterable,
            body,
        } => {
            check_default_expr(iterable, forbidden, has_self, scopes, diagnostics);
            scopes.push(pattern_bindings(pattern));
            check_body(body, forbidden, has_self, scopes, diagnostics);
            scopes.pop();
        }
        ExprKind::While {
            condition: iterable,
            body,
        }
        | ExprKind::Unless {
            condition: iterable,
            body,
        } => {
            check_default_expr(iterable, forbidden, has_self, scopes, diagnostics);
            check_body(body, forbidden, has_self, scopes, diagnostics);
        }
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            check_default_expr(condition, forbidden, has_self, scopes, diagnostics);
            check_body(then_body, forbidden, has_self, scopes, diagnostics);
            if let Some(body) = else_body {
                check_body(body, forbidden, has_self, scopes, diagnostics);
            }
        }
        ExprKind::List { elements } | ExprKind::Tuple { elements } => {
            for element in elements {
                check_default_expr(element, forbidden, has_self, scopes, diagnostics);
            }
        }
        ExprKind::Loop { body } => check_body(body, forbidden, has_self, scopes, diagnostics),
        ExprKind::Map { entries } => {
            for (key, value) in entries {
                for_exprs([key, value], forbidden, has_self, scopes, diagnostics);
            }
        }
        ExprKind::Match { subject, arms } => {
            check_default_expr(subject, forbidden, has_self, scopes, diagnostics);
            for arm in arms {
                scopes.push(pattern_bindings(&arm.pattern));
                if let Some(guard) = &arm.guard {
                    check_default_expr(guard, forbidden, has_self, scopes, diagnostics);
                }
                check_body(&arm.body, forbidden, has_self, scopes, diagnostics);
                scopes.pop();
            }
        }
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            for arm in arms {
                scopes.push(pattern_bindings(&arm.pattern));
                if let Some(guard) = &arm.guard {
                    check_default_expr(guard, forbidden, has_self, scopes, diagnostics);
                }
                check_body(&arm.body, forbidden, has_self, scopes, diagnostics);
                scopes.pop();
            }
            if let Some(timeout) = after_timeout {
                check_default_expr(timeout, forbidden, has_self, scopes, diagnostics);
            }
            check_body(after_body, forbidden, has_self, scopes, diagnostics);
        }
        ExprKind::Rescue {
            subject,
            binder,
            handler,
            ..
        } => {
            check_default_expr(subject, forbidden, has_self, scopes, diagnostics);
            scopes.push(binder.iter().cloned().collect());
            check_default_expr(handler, forbidden, has_self, scopes, diagnostics);
            scopes.pop();
        }
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    check_default_expr(expr, forbidden, has_self, scopes, diagnostics);
                }
            }
        }
        ExprKind::StructConstruction { fields, .. } => {
            for field in fields {
                check_default_expr(&field.value, forbidden, has_self, scopes, diagnostics);
            }
        }
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => for_exprs(
            [condition.as_ref(), then_expr.as_ref(), else_expr.as_ref()],
            forbidden,
            has_self,
            scopes,
            diagnostics,
        ),
        ExprKind::Ident { .. }
        | ExprKind::Literal { .. }
        | ExprKind::NamedFunctionReference { .. }
        | ExprKind::Self_ { .. } => {}
    }
}

fn closure_bindings(params: &[ClosureParam]) -> HashSet<String> {
    params
        .iter()
        .filter_map(|param| match param {
            ClosureParam::Name { name, .. } => Some(name.clone()),
            ClosureParam::Wildcard { .. } => None,
        })
        .collect()
}

fn pattern_bindings(pattern: &Pattern) -> HashSet<String> {
    let mut bindings = HashSet::new();
    collect_pattern_bindings(pattern, &mut bindings);
    bindings
}

fn collect_pattern_bindings(pattern: &Pattern, bindings: &mut HashSet<String>) {
    match pattern {
        Pattern::Binding { name, .. } | Pattern::TypedBinding { name, .. } => {
            bindings.insert(name.clone());
        }
        Pattern::Constructor { elements, .. }
        | Pattern::EnumTuple { elements, .. }
        | Pattern::List { elements, .. }
        | Pattern::Or {
            patterns: elements, ..
        }
        | Pattern::Tuple { elements, .. } => {
            for element in elements {
                collect_pattern_bindings(element, bindings);
            }
        }
        Pattern::EnumStruct { fields, .. } | Pattern::Struct { fields, .. } => {
            for field in fields {
                collect_pattern_bindings(&field.pattern, bindings);
            }
        }
        Pattern::Binary { segments, .. } => {
            for segment in segments {
                if let ExprKind::Ident { name, .. } = &segment.value.kind {
                    bindings.insert(name.clone());
                }
            }
        }
        Pattern::EnumUnit { .. } | Pattern::Literal { .. } | Pattern::Wildcard { .. } => {}
    }
}

fn check_body(
    body: &[Statement],
    forbidden: &HashSet<&str>,
    has_self: bool,
    scopes: &mut Vec<HashSet<String>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for statement in body {
        match statement {
            Statement::Assignment { value, .. }
            | Statement::CompoundAssign { value, .. }
            | Statement::Destructure { value, .. } => {
                check_default_expr(value, forbidden, has_self, scopes, diagnostics)
            }
            Statement::Expr(expr) => {
                check_default_expr(expr, forbidden, has_self, scopes, diagnostics)
            }
            Statement::Return {
                value: Some(value), ..
            } => check_default_expr(value, forbidden, has_self, scopes, diagnostics),
            Statement::Break { .. } | Statement::Return { value: None, .. } => {}
        }
    }
}

fn for_exprs<'a>(
    exprs: impl IntoIterator<Item = &'a Expr>,
    forbidden: &HashSet<&str>,
    has_self: bool,
    scopes: &mut Vec<HashSet<String>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for expr in exprs {
        check_default_expr(expr, forbidden, has_self, scopes, diagnostics);
    }
}
