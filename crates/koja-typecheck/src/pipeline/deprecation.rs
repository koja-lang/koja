//! Post-resolve deprecation warnings. Every use that resolves to a
//! `@deprecated` registry entry warns at the use span. Uses inside
//! the deprecated decl itself, and inside `impl` / `extend` blocks
//! whose target is deprecated, are suppressed so deprecating a type
//! doesn't flag its own methods.
//!
//! Expression uses read the [`Resolution::Global`] stamps resolve
//! left behind (idents, static receivers) or the [`ResolvedType`]
//! head on construction nodes. Type positions and patterns carry no
//! stamps, so their paths re-resolve through the same
//! [`lookup_type`] the resolver used.

use koja_ast::ast::{
    Annotation, AnnotationKind, BuiltinDecl, ClosureParam, Constant, Diagnostic,
    EnumConstructionData, EnumDecl, EnumVariantData, Expr, ExprKind, ExtendBlock, File, Function,
    ImplBlock, ImplMember, Item, Param, Pattern, ProtocolDecl, Statement, StringPart, StructDecl,
    StructField, TypeAlias, TypeExpr, TypeParam,
};
use koja_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;

use crate::pipeline::aliases::collect_file_aliases;
use crate::pipeline::collect::nominal_target_path;
use crate::pipeline::lift_signatures::ResolutionScope;
use crate::pipeline::resolve::types::{lookup_type, peel_alias};
use crate::registry::GlobalRegistry;

pub(crate) fn check_file(
    file: &File,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let aliases = collect_file_aliases(file);
    let scope = ResolutionScope {
        aliases: &aliases,
        package,
        registry,
    };
    let mut walker = Walker {
        diagnostics,
        scope,
        type_params: Vec::new(),
    };
    for item in &file.items {
        walker.check_item(item);
    }
    if let Some(body) = file.body.as_ref() {
        walker.check_body(body);
    }
}

/// Whether the decl carries a well-formed `@deprecated` annotation.
/// Its own body and signature never warn about other deprecated
/// items (or itself).
fn is_deprecated(annotations: &[Annotation]) -> bool {
    annotations
        .iter()
        .any(|a| matches!(a.kind(), AnnotationKind::Deprecated { .. }))
}

struct Walker<'a, 'd> {
    diagnostics: &'d mut Vec<Diagnostic>,
    scope: ResolutionScope<'a>,
    /// Generic-param names in scope, so a single-segment type path
    /// naming one is never misread as a global type.
    type_params: Vec<String>,
}

impl Walker<'_, '_> {
    fn check_item(&mut self, item: &Item) {
        match item {
            Item::Alias(_) => {}
            Item::Builtin(decl) => self.check_builtin(decl),
            Item::Constant(constant) => self.check_constant(constant),
            Item::Enum(decl) => self.check_enum(decl),
            Item::Extend(block) => self.check_extend(block),
            Item::Function(function) => self.check_function(function),
            Item::Impl(block) => self.check_impl(block),
            Item::Protocol(decl) => self.check_protocol(decl),
            Item::Struct(decl) => self.check_struct(decl),
            Item::TypeAlias(alias) => self.check_type_alias(alias),
        }
    }

    fn check_constant(&mut self, constant: &Constant) {
        if is_deprecated(&constant.annotations) {
            return;
        }
        if let Some(annotation) = constant.type_annotation.as_ref() {
            self.check_type_expr(annotation);
        }
        self.check_expr(&constant.value);
    }

    fn check_builtin(&mut self, decl: &BuiltinDecl) {
        if is_deprecated(&decl.annotations) {
            return;
        }
        self.with_type_params(&decl.type_params, |walker| {
            for function in &decl.functions {
                walker.check_function(function);
            }
        });
    }

    fn check_struct(&mut self, decl: &StructDecl) {
        if is_deprecated(&decl.annotations) {
            return;
        }
        self.with_type_params(&decl.type_params, |walker| {
            for field in &decl.fields {
                walker.check_struct_field(field);
            }
            for function in &decl.functions {
                walker.check_function(function);
            }
        });
    }

    fn check_enum(&mut self, decl: &EnumDecl) {
        if is_deprecated(&decl.annotations) {
            return;
        }
        self.with_type_params(&decl.type_params, |walker| {
            for variant in &decl.variants {
                match &variant.data {
                    EnumVariantData::Struct(fields) => {
                        for field in fields {
                            walker.check_struct_field(field);
                        }
                    }
                    EnumVariantData::Tuple(elements) => {
                        for element in elements {
                            walker.check_type_expr(element);
                        }
                    }
                    EnumVariantData::Unit => {}
                }
            }
            for function in &decl.functions {
                walker.check_function(function);
            }
        });
    }

    fn check_protocol(&mut self, decl: &ProtocolDecl) {
        if is_deprecated(&decl.annotations) {
            return;
        }
        self.with_type_params(&decl.type_params, |walker| {
            for method in &decl.methods {
                walker.with_type_params(&method.type_params, |walker| {
                    walker.check_params(&method.params);
                    if let Some(return_type) = method.return_type.as_ref() {
                        walker.check_type_expr(return_type);
                    }
                    if let Some(body) = method.body.as_ref() {
                        walker.check_body(body);
                    }
                });
            }
        });
    }

    fn check_type_alias(&mut self, alias: &TypeAlias) {
        if is_deprecated(&alias.annotations) {
            return;
        }
        self.check_type_expr(&alias.type_expr);
    }

    /// `impl Protocol for Target`. A deprecated target suppresses the
    /// whole block. Otherwise the protocol reference is itself a use.
    fn check_impl(&mut self, block: &ImplBlock) {
        if self.target_is_deprecated(&block.target) {
            return;
        }
        self.check_type_expr(&block.trait_expr);
        self.check_members(&block.members);
    }

    fn check_extend(&mut self, block: &ExtendBlock) {
        if self.target_is_deprecated(&block.target) {
            return;
        }
        self.check_members(&block.members);
    }

    fn check_members(&mut self, members: &[ImplMember]) {
        for member in members {
            match member {
                ImplMember::Function(function) => self.check_function(function),
                ImplMember::TypeAlias(alias) => self.check_type_alias(alias),
            }
        }
    }

    /// Whether an `impl`/`extend` target resolves to a deprecated
    /// entry, using the same path lookup collect keyed the block by.
    fn target_is_deprecated(&self, target: &TypeExpr) -> bool {
        let Some(path) = nominal_target_path(target) else {
            return false;
        };
        matches!(
            lookup_type(path, self.scope),
            Some((_, entry)) if entry.deprecation.is_some()
        )
    }

    fn check_function(&mut self, function: &Function) {
        if is_deprecated(&function.annotations) {
            return;
        }
        self.with_type_params(&function.type_params, |walker| {
            walker.check_params(&function.params);
            if let Some(return_type) = function.return_type.as_ref() {
                walker.check_type_expr(return_type);
            }
            if let Some(body) = function.body.as_ref() {
                walker.check_body(body);
            }
        });
    }

    fn check_params(&mut self, params: &[Param]) {
        for param in params {
            if let Param::Regular {
                type_expr, default, ..
            } = param
            {
                self.check_type_expr(type_expr);
                if let Some(default) = default.as_ref() {
                    self.check_expr(default);
                }
            }
        }
    }

    fn check_struct_field(&mut self, field: &StructField) {
        self.check_type_expr(&field.type_expr);
        if let Some(default) = field.default.as_ref() {
            self.check_expr(default);
        }
    }

    /// Push `params` (names and protocol bounds) for the duration of
    /// `walk`. Bounds are protocol references, so they warn too.
    fn with_type_params(&mut self, params: &[TypeParam], walk: impl FnOnce(&mut Self)) {
        let depth = self.type_params.len();
        for param in params {
            for bound in &param.bounds {
                self.check_type_expr(bound);
            }
            self.type_params.push(param.name.clone());
        }
        walk(self);
        self.type_params.truncate(depth);
    }

    fn check_body(&mut self, body: &[Statement]) {
        for stmt in body {
            self.check_statement(stmt);
        }
    }

    fn check_statement(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Assignment {
                type_annotation,
                value,
                ..
            } => {
                if let Some(annotation) = type_annotation.as_ref() {
                    self.check_type_expr(annotation);
                }
                self.check_expr(value);
            }
            Statement::Break { .. } | Statement::Return { value: None, .. } => {}
            Statement::CompoundAssign { value, .. } => self.check_expr(value),
            Statement::Destructure { pattern, value, .. } => {
                self.check_pattern(pattern);
                self.check_expr(value);
            }
            Statement::Expr(expr) => self.check_expr(expr),
            Statement::Return {
                value: Some(value), ..
            } => self.check_expr(value),
        }
    }

    fn check_expr(&mut self, expr: &Expr) {
        // Compiler-synthesized subtrees (field-default fills, derived
        // impls) reuse user expressions that already warned at their
        // declaration. Warning again per site would be noise.
        if expr.span.synthetic {
            return;
        }
        match &expr.kind {
            ExprKind::Binary { left, right, .. } => {
                self.check_expr(left);
                self.check_expr(right);
            }
            ExprKind::BinaryLiteral { segments } => {
                for segment in segments {
                    self.check_expr(&segment.value);
                    if let Some(size) = segment.size.as_ref() {
                        self.check_expr(size);
                    }
                }
            }
            ExprKind::Call { callee, args, .. } => {
                self.check_expr(callee);
                for arg in args {
                    self.check_expr(&arg.value);
                }
            }
            ExprKind::Closure {
                params,
                return_type,
                body,
            } => {
                for param in params {
                    if let ClosureParam::Name {
                        type_expr: Some(type_expr),
                        ..
                    } = param
                    {
                        self.check_type_expr(type_expr);
                    }
                }
                if let Some(return_type) = return_type.as_ref() {
                    self.check_type_expr(return_type);
                }
                self.check_body(body);
            }
            ExprKind::Cond { arms, else_body } => {
                for arm in arms {
                    self.check_expr(&arm.condition);
                    self.check_body(&arm.body);
                }
                if let Some(else_body) = else_body {
                    self.check_body(else_body);
                }
            }
            ExprKind::EnumConstruction { data, .. } => {
                self.warn_resolution_head(&expr.resolution, expr.span);
                match data {
                    EnumConstructionData::Struct(fields) => {
                        for field in fields {
                            self.check_expr(&field.value);
                        }
                    }
                    EnumConstructionData::Tuple(elements) => {
                        for element in elements {
                            self.check_expr(element);
                        }
                    }
                    EnumConstructionData::Unit => {}
                }
            }
            ExprKind::Fail { value } => self.check_expr(value),
            ExprKind::FieldAccess { receiver, .. } => self.check_expr(receiver),
            ExprKind::For {
                pattern,
                iterable,
                body,
            } => {
                self.check_pattern(pattern);
                self.check_expr(iterable);
                self.check_body(body);
            }
            ExprKind::Group { expr: inner } | ExprKind::Spawn { expr: inner } => {
                self.check_expr(inner);
            }
            ExprKind::Ident {
                resolution: Resolution::Global(id),
                ..
            } => self.warn_use(*id, expr.span),
            ExprKind::Ident { .. } | ExprKind::Literal { .. } | ExprKind::Self_ { .. } => {}
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                self.check_expr(condition);
                self.check_body(then_body);
                if let Some(else_body) = else_body {
                    self.check_body(else_body);
                }
            }
            ExprKind::List { elements } | ExprKind::Tuple { elements } => {
                for element in elements {
                    self.check_expr(element);
                }
            }
            ExprKind::Loop { body } => self.check_body(body),
            ExprKind::Map { entries } => {
                for (key, value) in entries {
                    self.check_expr(key);
                    self.check_expr(value);
                }
            }
            ExprKind::Match { subject, arms } => {
                self.check_expr(subject);
                for arm in arms {
                    self.check_pattern(&arm.pattern);
                    if let Some(guard) = arm.guard.as_ref() {
                        self.check_expr(guard);
                    }
                    self.check_body(&arm.body);
                }
            }
            ExprKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } => {
                self.warn_deprecated_method(receiver, method, expr.span);
                self.check_expr(receiver);
                for arg in args {
                    self.check_expr(&arg.value);
                }
            }
            ExprKind::Receive {
                arms,
                after_timeout,
                after_body,
            } => {
                for arm in arms {
                    self.check_pattern(&arm.pattern);
                    if let Some(guard) = arm.guard.as_ref() {
                        self.check_expr(guard);
                    }
                    self.check_body(&arm.body);
                }
                if let Some(timeout) = after_timeout.as_ref() {
                    self.check_expr(timeout);
                }
                self.check_body(after_body);
            }
            ExprKind::Rescue {
                subject, handler, ..
            } => {
                self.check_expr(subject);
                self.check_expr(handler);
            }
            ExprKind::ShortClosure { body, .. } => self.check_expr(body),
            ExprKind::String { parts, .. } => {
                for part in parts {
                    if let StringPart::Interpolation { expr: inner, .. } = part {
                        self.check_expr(inner);
                    }
                }
            }
            ExprKind::StructConstruction { fields, .. } => {
                self.warn_resolution_head(&expr.resolution, expr.span);
                for field in fields {
                    self.check_expr(&field.value);
                }
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.check_expr(condition);
                self.check_expr(then_expr);
                self.check_expr(else_expr);
            }
            ExprKind::Try { expr: inner } => self.check_expr(inner),
            ExprKind::Unary { operand, .. } => self.check_expr(operand),
            ExprKind::Unless { condition, body } | ExprKind::While { condition, body } => {
                self.check_expr(condition);
                self.check_body(body);
            }
        }
    }

    fn check_pattern(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::Binary { .. }
            | Pattern::Binding { .. }
            | Pattern::Literal { .. }
            | Pattern::Wildcard { .. } => {}
            Pattern::Constructor { elements, .. } => {
                for element in elements {
                    self.check_pattern(element);
                }
            }
            Pattern::EnumStruct {
                type_path,
                fields,
                span,
                ..
            }
            | Pattern::Struct {
                type_path,
                fields,
                span,
            } => {
                self.warn_type_path(type_path, *span);
                for field in fields {
                    self.check_pattern(&field.pattern);
                }
            }
            Pattern::EnumTuple {
                type_path,
                elements,
                span,
                ..
            } => {
                self.warn_type_path(type_path, *span);
                for element in elements {
                    self.check_pattern(element);
                }
            }
            Pattern::EnumUnit {
                type_path, span, ..
            } => self.warn_type_path(type_path, *span),
            Pattern::List { elements, .. }
            | Pattern::Or {
                patterns: elements, ..
            }
            | Pattern::Tuple { elements, .. } => {
                for element in elements {
                    self.check_pattern(element);
                }
            }
            Pattern::TypedBinding { type_expr, .. } => self.check_type_expr(type_expr),
        }
    }

    fn check_type_expr(&mut self, type_expr: &TypeExpr) {
        match type_expr {
            TypeExpr::Function {
                params,
                return_type,
                ..
            } => {
                for param in params {
                    self.check_type_expr(param);
                }
                self.check_type_expr(return_type);
            }
            TypeExpr::Generic { path, args, span } => {
                self.warn_type_path(path, *span);
                for arg in args {
                    self.check_type_expr(arg);
                }
            }
            TypeExpr::Named { path, span } => self.warn_type_path(path, *span),
            TypeExpr::Self_ { .. } | TypeExpr::Unit { .. } => {}
            TypeExpr::Tuple { elements, .. } => {
                for element in elements {
                    self.check_type_expr(element);
                }
            }
            TypeExpr::Union { types, .. } => {
                for member in types {
                    self.check_type_expr(member);
                }
            }
        }
    }

    /// Warn when a source type path names a deprecated entry.
    /// In-scope generic params shadow globals, so those never warn.
    fn warn_type_path(&mut self, path: &[String], span: Span) {
        if path.len() == 1 && self.type_params.contains(&path[0]) {
            return;
        }
        let Some((id, _)) = lookup_type(path, self.scope) else {
            return;
        };
        self.warn_use(id, span);
    }

    /// Warn when a method call lands on a deprecated function entry.
    /// The receiver's own deprecation is warned separately, by the
    /// `Ident` hook for statics or wherever the value was produced
    /// for instances.
    fn warn_deprecated_method(&mut self, receiver: &Expr, method: &str, span: Span) {
        let type_id = match &receiver.kind {
            ExprKind::Ident {
                resolution: Resolution::Global(id),
                ..
            } => Some(*id),
            _ => match peel_alias(&receiver.resolution, self.scope.registry) {
                ResolvedType::Named {
                    resolution: Resolution::Global(id),
                    ..
                } => Some(id),
                _ => None,
            },
        };
        let Some(type_id) = type_id else {
            return;
        };
        let Some(type_entry) = self.scope.registry.get(type_id) else {
            return;
        };
        let method_identifier = Identifier::member(
            type_entry.identifier.package(),
            type_entry.identifier.path(),
            method,
        );
        let Some((method_id, _)) = self.scope.registry.lookup(&method_identifier) else {
            return;
        };
        self.warn_use(method_id, span);
    }

    /// Warn when a construction expression's resolved head names a
    /// deprecated type.
    fn warn_resolution_head(&mut self, resolution: &ResolvedType, span: Span) {
        if let ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } = resolution
        {
            self.warn_use(*id, span);
        }
    }

    fn warn_use(&mut self, id: GlobalRegistryId, span: Span) {
        let Some(entry) = self.scope.registry.get(id) else {
            return;
        };
        let Some(message) = entry.deprecation.as_ref() else {
            return;
        };
        // The LSP keys its deprecated-tag detection off this message
        // shape (koja-lsp's `is_deprecation_warning`). Keep the two
        // in sync when changing the wording.
        self.diagnostics.push(Diagnostic::warning(
            format!(
                "`{}` is deprecated: {message}",
                entry.identifier.path().join("."),
            ),
            span,
        ));
    }
}
