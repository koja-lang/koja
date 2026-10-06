//! Normalize default parameters into exact-arity adapter functions.

use std::collections::HashSet;

use koja_ast::ast::{
    AnnotationKind, Arg, ClosureParam, Diagnostic, Expr, ExprKind, Function, FunctionOrigin,
    ImplMember, Item, MatchArm, Name, Param, Pattern, ProtocolMethod, Statement, TypeExpr,
    TypeParam,
};
use koja_ast::identifier::Resolution;
use koja_ast::visit::{self, Visitor};

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
    let mut walker = Walker {
        diagnostics,
        forbidden,
        has_self,
        scopes: Vec::new(),
    };
    for param in params {
        let Param::Regular {
            default: Some(default),
            ..
        } = param
        else {
            continue;
        };
        walker.visit_expr(default);
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
                name: Name::new(&function.name.text, function.name.span.as_synthetic()),
                type_params: adapter_type_params(
                    &function.type_params,
                    &function.params[..arity],
                    function.return_type.as_ref(),
                    function.error_type.as_ref(),
                ),
                params: explicit_params(&function.params, arity),
                return_type: function.return_type.clone(),
                error_type: function.error_type.clone(),
                body: Some(adapter_body(
                    function.name.as_str(),
                    &function.params,
                    arity,
                    function.error_type.is_some(),
                    span,
                )),
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
                name: Name::new(&method.name.text, method.name.span.as_synthetic()),
                type_params: adapter_type_params(
                    &method.type_params,
                    &method.params[..arity],
                    method.return_type.as_ref(),
                    method.error_type.as_ref(),
                ),
                params: explicit_params(&method.params, arity),
                return_type: method.return_type.clone(),
                error_type: method.error_type.clone(),
                body: Some(adapter_body(
                    method.name.as_str(),
                    &method.params,
                    arity,
                    method.error_type.is_some(),
                    span,
                )),
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

/// The type parameters an adapter of `arity` still needs. A type
/// parameter that only the omitted parameters mention is dropped,
/// since the default expression fixes its type and the canonical
/// call infers it from there. Keeping it would leave the adapter
/// with a type parameter no argument can bind, and every call would
/// fail with "cannot infer type parameter". A parameter that the kept
/// parameters, the return type, the error type, or the bound of
/// another kept parameter mention stays.
fn adapter_type_params(
    type_params: &[TypeParam],
    kept: &[Param],
    return_type: Option<&TypeExpr>,
    error_type: Option<&TypeExpr>,
) -> Vec<TypeParam> {
    if type_params.is_empty() {
        return Vec::new();
    }
    let mut mentioned = HashSet::new();
    for param in kept {
        if let Param::Regular { type_expr, .. } = param {
            collect_type_names(type_expr, &mut mentioned);
        }
    }
    for type_expr in return_type.into_iter().chain(error_type) {
        collect_type_names(type_expr, &mut mentioned);
    }
    // A kept parameter's bound can name another type parameter
    // (`<T, F: Format<T>>`), so close over bounds until nothing new
    // appears.
    loop {
        let before = mentioned.len();
        for type_param in type_params {
            if mentioned.contains(type_param.name.as_str()) {
                for bound in &type_param.bounds {
                    collect_type_names(bound, &mut mentioned);
                }
            }
        }
        if mentioned.len() == before {
            break;
        }
    }
    type_params
        .iter()
        .filter(|type_param| mentioned.contains(type_param.name.as_str()))
        .cloned()
        .collect()
}

/// Every single-segment type name a type expression mentions, which
/// is the only shape a type parameter reference can take.
fn collect_type_names(type_expr: &TypeExpr, out: &mut HashSet<String>) {
    match type_expr {
        TypeExpr::Named { path, .. } => {
            if let [name] = path.as_slice() {
                out.insert(name.text.clone());
            }
        }
        TypeExpr::Generic { path, args, .. } => {
            if let [name] = path.as_slice() {
                out.insert(name.text.clone());
            }
            for arg in args {
                collect_type_names(arg, out);
            }
        }
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            for param in params {
                collect_type_names(param, out);
            }
            collect_type_names(return_type, out);
        }
        TypeExpr::Tuple { elements, .. }
        | TypeExpr::Union {
            types: elements, ..
        } => {
            for element in elements {
                collect_type_names(element, out);
            }
        }
        TypeExpr::Unit { .. } | TypeExpr::Self_ { .. } => {}
    }
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
    fallible: bool,
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
                        name: name.text.clone(),
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
            method: Name::new(name, span),
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
    // A fallible adapter forwards through `try` because the canonical
    // call yields the wrapped `Result` while the adapter's own
    // Ok-autowrap expects the plain success value.
    let call = Expr::new(kind, span);
    let expr = if fallible {
        Expr::new(
            ExprKind::Try {
                expr: Box::new(call),
            },
            span,
        )
    } else {
        call
    };
    vec![Statement::Expr(expr)]
}

fn adapter_annotations(
    annotations: &[koja_ast::ast::Annotation],
) -> Vec<koja_ast::ast::Annotation> {
    annotations
        .iter()
        .filter(|annotation| {
            matches!(
                annotation.kind(),
                AnnotationKind::Deprecated { .. } | AnnotationKind::Experimental { .. }
            )
        })
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

/// Recursion state for the default expressions of one parameter
/// list. `forbidden` holds the sibling parameter names a default
/// may not read, `has_self` says whether `self` is one of them, and
/// `scopes` tracks the closure and pattern bindings that shadow a
/// forbidden name on the way down.
struct Walker<'a> {
    diagnostics: &'a mut Vec<Diagnostic>,
    forbidden: HashSet<&'a str>,
    has_self: bool,
    scopes: Vec<HashSet<String>>,
}

impl Walker<'_> {
    /// Run `walk` inside a scope that rebinds `names`.
    fn in_scope(&mut self, names: HashSet<String>, walk: impl FnOnce(&mut Self)) {
        self.scopes.push(names);
        walk(self);
        self.scopes.pop();
    }

    /// Whether a closure param or pattern binding on the way down
    /// rebinds `name`, so the ident reads that binding and not the
    /// parameter.
    fn is_shadowed(&self, name: &str) -> bool {
        self.scopes.iter().rev().any(|scope| scope.contains(name))
    }
}

impl<'ast> Visitor<'ast> for Walker<'_> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match &expr.kind {
            ExprKind::Ident { name, .. }
                if self.forbidden.contains(name.as_str()) && !self.is_shadowed(name) =>
            {
                self.diagnostics.push(Diagnostic::error(
                    format!("default parameter value cannot reference parameter `{name}`"),
                    expr.span,
                ));
            }
            ExprKind::Self_ { .. } if self.has_self => self.diagnostics.push(Diagnostic::error(
                "default parameter value cannot reference `self`".to_string(),
                expr.span,
            )),
            ExprKind::Closure { params, .. } | ExprKind::ShortClosure { params, .. } => {
                self.in_scope(closure_bindings(params), |w| visit::walk_expr(w, expr));
            }
            // The iterable is read before the pattern binds.
            ExprKind::For {
                pattern,
                iterable,
                body,
            } => {
                self.visit_expr(iterable);
                self.in_scope(pattern_bindings(pattern), |w| visit::walk_body(w, body));
            }
            // The binder is in scope for the handler only.
            ExprKind::Rescue {
                subject,
                binder,
                handler,
                ..
            } => {
                self.visit_expr(subject);
                self.in_scope(binder.iter().cloned().collect(), |w| w.visit_expr(handler));
            }
            _ => visit::walk_expr(self, expr),
        }
    }

    fn visit_match_arm(&mut self, arm: &'ast MatchArm) {
        self.in_scope(pattern_bindings(&arm.pattern), |w| {
            visit::walk_match_arm(w, arm)
        });
    }

    /// Patterns bind names and never read them.
    fn visit_pattern(&mut self, _pattern: &'ast Pattern) {}
}

fn closure_bindings(params: &[ClosureParam]) -> HashSet<String> {
    params
        .iter()
        .filter_map(|param| match param {
            ClosureParam::Name { name, .. } => Some(name.text.clone()),
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
            bindings.insert(name.text.clone());
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
