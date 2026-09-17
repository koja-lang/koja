//! The reference index. One pass over the files records every place
//! a symbol is declared, read, or written, keyed by what typecheck
//! resolved it to. Find references, rename, document highlight, and
//! position lookups are all queries over this table.
//!
//! Every occurrence comes from a stamp typecheck wrote. The stamps
//! are `Resolution` on identifiers, method calls, function
//! references, and type paths, `LocalId` on bindings, and the head of
//! `Expr.resolution` on constructions. Declarations map to the
//! registry through their
//! canonical `Identifier`, which is the direction the registry is
//! keyed in. Nothing here resolves a name by scope rules.
//!
//! Synthesized names carry synthetic spans and are skipped, so the
//! `eq` behind `a == b` or the `Option` behind a `for` loop never
//! shows up as a reference.

use std::collections::HashMap;

use koja_ast::ast::*;
use koja_ast::identifier::{
    GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType, TypeParamIndex,
};
use koja_ast::span::{FileId, Span};
use koja_typecheck::{GlobalKind, GlobalRegistry};

use crate::Analysis;
use crate::position::{span_contains, tail_segment_span};
use crate::visit::{self, Visitor};

/// Identity of a symbol across every file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SymbolKey {
    /// A registry entry, so a function, method, type, constant,
    /// alias, or protocol.
    Global(GlobalRegistryId),
    /// A local binding. Typecheck restarts local ids for each
    /// function, so the enclosing function span disambiguates.
    Local { scope: Span, id: LocalId },
    /// A type parameter of the declaration `owner`.
    TypeParam {
        owner: GlobalRegistryId,
        index: TypeParamIndex,
    },
}

/// What an occurrence does with its symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Declaration,
    Read,
    Write,
}

/// One mention of a symbol in source.
#[derive(Clone, Debug)]
pub struct Occurrence {
    pub key: SymbolKey,
    /// The name token only, never the enclosing node.
    pub span: Span,
    pub role: Role,
    /// The identifier text at `span`.
    pub name: String,
    /// The type typecheck assigned at this site, when it assigned
    /// one. Local declarations carry the initializer's type and
    /// identifier reads carry the expression's type.
    pub ty: Option<ResolvedType>,
}

/// Every occurrence in the indexed files, with lookups by key, by
/// file, and by declaration.
#[derive(Debug, Default)]
pub struct ReferenceIndex {
    occurrences: Vec<Occurrence>,
    by_key: HashMap<SymbolKey, Vec<usize>>,
    by_file: HashMap<FileId, Vec<usize>>,
    declarations: HashMap<SymbolKey, usize>,
}

impl ReferenceIndex {
    /// Index every file in `analysis`.
    pub fn build(analysis: &Analysis<'_>) -> Self {
        Self::build_filtered(analysis, |_| true)
    }

    /// Index the files `include` accepts. A reference to a symbol
    /// declared in a skipped file still resolves, because keys are
    /// registry ids and [`Self::declaration_span`] falls back to the
    /// registry entry.
    pub fn build_filtered(analysis: &Analysis<'_>, mut include: impl FnMut(&File) -> bool) -> Self {
        let mut builder = Builder {
            registry: analysis.registry,
            package: "",
            scope: None,
            owner: None,
            type_param_owner: None,
            signature_params: None,
            index: ReferenceIndex::default(),
        };
        for file in analysis.files() {
            if include(file) {
                builder.package = &file.package;
                builder.visit_file(file);
            }
        }
        builder.index
    }

    pub fn len(&self) -> usize {
        self.occurrences.len()
    }

    pub fn is_empty(&self) -> bool {
        self.occurrences.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Occurrence> {
        self.occurrences.iter()
    }

    /// The smallest occurrence in `file` that contains the 1-indexed
    /// cursor position.
    pub fn occurrence_at(&self, file: FileId, line: u32, col: u32) -> Option<&Occurrence> {
        self.by_file
            .get(&file)?
            .iter()
            .map(|&i| &self.occurrences[i])
            .filter(|occurrence| span_contains(&occurrence.span, line, col))
            .min_by_key(|occurrence| span_size(&occurrence.span))
    }

    /// Every occurrence of `key`, in file order.
    pub fn occurrences(&self, key: SymbolKey) -> impl Iterator<Item = &Occurrence> {
        self.by_key
            .get(&key)
            .into_iter()
            .flatten()
            .map(|&i| &self.occurrences[i])
    }

    /// The declaring occurrence of `key`, when its file was indexed.
    pub fn declaration(&self, key: SymbolKey) -> Option<&Occurrence> {
        self.declarations.get(&key).map(|&i| &self.occurrences[i])
    }

    /// Where `key` is declared. Falls back to the registry entry's
    /// name span for a global whose file was not indexed, such as a
    /// stdlib declaration.
    pub fn declaration_span(&self, key: SymbolKey, registry: &GlobalRegistry) -> Option<Span> {
        if let Some(declaration) = self.declaration(key) {
            return Some(declaration.span);
        }
        match key {
            SymbolKey::Global(id) => {
                let span = registry.get(id)?.name_span;
                (!span.synthetic).then_some(span)
            }
            SymbolKey::Local { .. } | SymbolKey::TypeParam { .. } => None,
        }
    }

    fn push(&mut self, occurrence: Occurrence) {
        if occurrence.span.synthetic {
            return;
        }
        let i = self.occurrences.len();
        self.by_key.entry(occurrence.key).or_default().push(i);
        self.by_file
            .entry(occurrence.span.file)
            .or_default()
            .push(i);
        if occurrence.role == Role::Declaration {
            self.declarations.entry(occurrence.key).or_insert(i);
        }
        self.occurrences.push(occurrence);
    }
}

fn span_size(span: &Span) -> (u32, u32) {
    (
        span.end.line - span.start.line,
        span.end.column.wrapping_sub(span.start.column),
    )
}

/// Visitor that fills a [`ReferenceIndex`]. The fields other than
/// `index` are the walk context, so which package and function we
/// are in and which declaration owns the type parameters in scope.
struct Builder<'a> {
    registry: &'a GlobalRegistry,
    package: &'a str,
    /// Span of the enclosing function, test, or script body. Local
    /// keys need it. `None` outside any body.
    scope: Option<Span>,
    /// Identifier of the type whose inline methods, impl members, or
    /// extend members we are walking.
    owner: Option<Identifier>,
    /// The declaration whose type parameters are in scope.
    type_param_owner: Option<GlobalRegistryId>,
    /// Resolved parameter types of the enclosing function, indexed
    /// like its `params`.
    signature_params: Option<Vec<ResolvedType>>,
    index: ReferenceIndex,
}

impl<'a> Builder<'a> {
    fn lookup(&self, identifier: &Identifier) -> Option<GlobalRegistryId> {
        self.registry.lookup(identifier).map(|(id, _)| id)
    }

    /// The key a `Resolution` stamp denotes, or `None` when the stamp
    /// is unresolved, points at a missing entry, or is a local
    /// outside any body.
    fn key_for(&self, resolution: Resolution) -> Option<SymbolKey> {
        match resolution {
            Resolution::Global(id) => self.canonical_global(id).map(SymbolKey::Global),
            Resolution::Local(id) => self.scope.map(|scope| SymbolKey::Local { scope, id }),
            Resolution::TypeParam { owner, index } => Some(SymbolKey::TypeParam { owner, index }),
            Resolution::Unresolved => None,
        }
    }

    /// A call with defaults omitted targets a synthesized adapter.
    /// Fold it onto the declared function so every call site shares
    /// one key.
    fn canonical_global(&self, id: GlobalRegistryId) -> Option<GlobalRegistryId> {
        let entry = self.registry.get(id)?;
        if let GlobalKind::Function(definition) = &entry.kind
            && let FunctionOrigin::DefaultAdapter { canonical_arity } = definition.origin
            && let Some((canonical, _)) = self
                .registry
                .lookup_function(&entry.identifier, canonical_arity)
        {
            return Some(canonical);
        }
        Some(id)
    }

    fn local_key(&self, id: Option<LocalId>) -> Option<SymbolKey> {
        let id = id?;
        self.scope.map(|scope| SymbolKey::Local { scope, id })
    }

    fn record(&mut self, key: SymbolKey, name: &Name, role: Role, ty: Option<ResolvedType>) {
        self.index.push(Occurrence {
            key,
            span: name.span,
            role,
            name: name.text.clone(),
            ty,
        });
    }

    fn record_global_declaration(&mut self, id: GlobalRegistryId, name: &Name) {
        self.record(SymbolKey::Global(id), name, Role::Declaration, None);
    }

    /// A local binding declares on first sight and writes after.
    /// Destructuring may rebind an existing local under the same id.
    fn record_local_binding(&mut self, id: Option<LocalId>, name: &Name, ty: Option<ResolvedType>) {
        let Some(key) = self.local_key(id) else {
            return;
        };
        let role = if self.index.declarations.contains_key(&key) {
            Role::Write
        } else {
            Role::Declaration
        };
        self.record(key, name, role, ty);
    }

    fn record_type_path(&mut self, path: &[Name], resolution: Resolution) {
        let Some(name) = path.last() else {
            return;
        };
        if let Some(key) = self.key_for(resolution) {
            self.record(key, name, Role::Read, None);
        }
    }

    /// The type a construction builds, read from the expression's
    /// stamp. Constructions carry no path stamp of their own.
    fn record_construction(&mut self, type_path: &[Name], expr: &Expr) {
        if let ResolvedType::Named {
            resolution: Resolution::Global(id),
            ..
        } = &expr.resolution
            && let Some(name) = type_path.last()
        {
            self.record(SymbolKey::Global(*id), name, Role::Read, None);
        }
    }

    fn record_type_param_declarations(&mut self, owner: GlobalRegistryId, params: &[TypeParam]) {
        let Some(names) = self.registry.type_params(owner) else {
            return;
        };
        let names: Vec<String> = names.to_vec();
        for param in params {
            if let Some(index) = names.iter().position(|n| *n == param.name.text) {
                let key = SymbolKey::TypeParam {
                    owner,
                    index: TypeParamIndex::new(index as u32),
                };
                self.record(key, &param.name, Role::Declaration, None);
            }
        }
    }

    fn record_lvalue(&mut self, target: &LValue, value: Option<&Expr>) {
        let Some(head) = target.segments.first() else {
            return;
        };
        if target.segments.len() == 1 {
            let ty = value
                .filter(|v| v.resolution.is_resolved())
                .map(|v| v.resolution.clone());
            self.record_local_binding(target.local_id, head, ty);
        } else if let Some(key) = self.local_key(target.local_id) {
            self.record(key, head, Role::Write, None);
        }
    }

    fn type_identifier(&self, path: &[Name]) -> Identifier {
        Identifier::new(self.package, name_texts(path))
    }

    fn function_identifier(&self, name: &Name) -> Identifier {
        match &self.owner {
            Some(owner) => Identifier::member(owner.package(), owner.path(), name.as_str()),
            None => Identifier::single(self.package, name.as_str()),
        }
    }

    /// Walk a type declaration's members with the type as owner and
    /// type parameter scope, then restore the outer context.
    fn with_type_owner(&mut self, id: Option<GlobalRegistryId>, walk: impl FnOnce(&mut Self)) {
        let saved_owner = self.owner.take();
        let saved_type_param_owner = self.type_param_owner;
        self.owner = id.and_then(|id| self.registry.get(id).map(|e| e.identifier.clone()));
        self.type_param_owner = id;
        walk(self);
        self.owner = saved_owner;
        self.type_param_owner = saved_type_param_owner;
    }

    fn with_body_scope(&mut self, scope: Span, walk: impl FnOnce(&mut Self)) {
        let saved = self.scope.replace(scope);
        walk(self);
        self.scope = saved;
    }

    /// The type entry an `impl` or `extend` target names, from its
    /// path stamp.
    fn target_type_id(target: &TypeExpr) -> Option<GlobalRegistryId> {
        match target {
            TypeExpr::Named {
                resolution: Resolution::Global(id),
                ..
            }
            | TypeExpr::Generic {
                resolution: Resolution::Global(id),
                ..
            } => Some(*id),
            _ => None,
        }
    }

    fn record_params(&mut self, params: &[Param]) {
        for (i, param) in params.iter().enumerate() {
            let ty = self
                .signature_params
                .as_ref()
                .and_then(|types| types.get(i).cloned());
            match param {
                Param::Regular { name, local_id, .. } => {
                    self.record_local_binding(*local_id, name, ty);
                }
                Param::Self_ { local_id, span } => {
                    let name = Name::new("self", *span);
                    self.record_local_binding(*local_id, &name, ty);
                }
            }
        }
    }
}

impl<'ast> Visitor<'ast> for Builder<'_> {
    fn visit_file(&mut self, file: &'ast File) {
        for item in &file.items {
            self.visit_item(item);
        }
        if let Some(body) = &file.body {
            self.with_body_scope(file.span, |b| visit::walk_body(b, body));
        }
    }

    fn visit_item(&mut self, item: &'ast Item) {
        match item {
            Item::Alias(alias) => {
                let Some((package, rest)) = alias.path.split_first() else {
                    return;
                };
                if rest.is_empty() {
                    return;
                }
                let identifier = Identifier::new(package.as_str(), name_texts(rest));
                let tail = &alias.path[alias.path.len() - 1];
                // A function alias binds every arity, so the alias
                // line is a reference to each one.
                let mut ids: Vec<GlobalRegistryId> = self
                    .registry
                    .function_arities(&identifier)
                    .into_iter()
                    .filter_map(|arity| self.registry.lookup_function(&identifier, arity))
                    .map(|(id, _)| id)
                    .collect();
                if ids.is_empty() {
                    ids.extend(self.lookup(&identifier));
                }
                for id in ids {
                    self.record(SymbolKey::Global(id), tail, Role::Read, None);
                }
            }
            Item::Builtin(decl) => {
                let id = self.lookup(&self.type_identifier(&decl.path));
                if let Some(id) = id {
                    self.record_global_declaration(id, decl.name());
                    self.record_type_param_declarations(id, &decl.type_params);
                }
                self.with_type_owner(id, |b| visit::walk_item(b, item));
            }
            Item::Constant(constant) => {
                if let Some(id) =
                    self.lookup(&Identifier::single(self.package, constant.name.as_str()))
                {
                    self.record_global_declaration(id, &constant.name);
                }
                visit::walk_item(self, item);
            }
            Item::Enum(decl) => {
                let id = self.lookup(&self.type_identifier(&decl.path));
                if let Some(id) = id {
                    self.record_global_declaration(id, decl.name());
                    self.record_type_param_declarations(id, &decl.type_params);
                }
                self.with_type_owner(id, |b| visit::walk_item(b, item));
            }
            Item::Struct(decl) => {
                let id = self.lookup(&self.type_identifier(&decl.path));
                if let Some(id) = id {
                    self.record_global_declaration(id, decl.name());
                    self.record_type_param_declarations(id, &decl.type_params);
                }
                self.with_type_owner(id, |b| visit::walk_item(b, item));
            }
            Item::Extend(block) => {
                let id = Self::target_type_id(&block.target);
                self.with_type_owner(id, |b| {
                    b.type_param_owner = None;
                    b.record_member_aliases(&block.members);
                    visit::walk_item(b, item);
                });
            }
            Item::Impl(block) => {
                let id = Self::target_type_id(&block.target);
                self.with_type_owner(id, |b| {
                    b.type_param_owner = None;
                    b.record_member_aliases(&block.members);
                    visit::walk_item(b, item);
                });
            }
            Item::Function(_) => visit::walk_item(self, item),
            Item::Protocol(decl) => {
                let id = self.lookup(&self.type_identifier(&decl.path));
                if let Some(id) = id {
                    self.record_global_declaration(id, decl.name());
                    self.record_type_param_declarations(id, &decl.type_params);
                }
                self.with_type_owner(id, |b| visit::walk_item(b, item));
            }
            Item::Test(test) => {
                self.with_body_scope(test.span, |b| visit::walk_item(b, item));
            }
            Item::TypeAlias(alias) => {
                if let Some(id) =
                    self.lookup(&Identifier::single(self.package, alias.name.as_str()))
                {
                    self.record_global_declaration(id, &alias.name);
                }
                visit::walk_item(self, item);
            }
        }
    }

    fn visit_function(&mut self, function: &'ast Function) {
        if matches!(function.origin, FunctionOrigin::DefaultAdapter { .. }) {
            return;
        }
        let identifier = self.function_identifier(&function.name);
        let registry = self.registry;
        let looked_up = registry.lookup_function(&identifier, function.params.len());
        let id = looked_up.map(|(id, _)| id);
        let signature_params = looked_up.and_then(|(_, entry)| {
            entry
                .function_definition()?
                .signature
                .as_ref()
                .map(|sig| sig.params.iter().map(|p| p.ty.clone()).collect())
        });
        if let Some(id) = id {
            self.record_global_declaration(id, &function.name);
            self.record_type_param_declarations(id, &function.type_params);
        }

        let saved_type_param_owner = self.type_param_owner;
        let saved_signature_params = self.signature_params.take();
        if id.is_some() {
            self.type_param_owner = id;
        }
        self.signature_params = signature_params;
        self.with_body_scope(function.span, |b| {
            b.record_params(&function.params);
            visit::walk_function(b, function);
        });
        self.type_param_owner = saved_type_param_owner;
        self.signature_params = saved_signature_params;
    }

    fn visit_protocol_method(&mut self, method: &'ast ProtocolMethod) {
        let saved_signature_params = self.signature_params.take();
        self.with_body_scope(method.span, |b| {
            b.record_params(&method.params);
            visit::walk_protocol_method(b, method);
        });
        self.signature_params = saved_signature_params;
    }

    fn visit_closure_param(&mut self, param: &'ast ClosureParam) {
        if let ClosureParam::Name { local_id, name, .. } = param {
            self.record_local_binding(*local_id, name, None);
        }
        visit::walk_closure_param(self, param);
    }

    fn visit_statement(&mut self, statement: &'ast Statement) {
        match statement {
            Statement::Assignment { target, value, .. } => self.record_lvalue(target, Some(value)),
            Statement::CompoundAssign { target, .. } => self.record_lvalue(target, None),
            _ => {}
        }
        visit::walk_statement(self, statement);
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        match &expr.kind {
            ExprKind::Ident { name, resolution } => {
                if let Some(key) = self.key_for(*resolution) {
                    let span = if name.contains('.') {
                        tail_segment_span(expr.span, name)
                    } else {
                        expr.span
                    };
                    let tail = name.rsplit('.').next().unwrap_or(name);
                    let ty = expr
                        .resolution
                        .is_resolved()
                        .then(|| expr.resolution.clone());
                    self.index.push(Occurrence {
                        key,
                        span,
                        role: Role::Read,
                        name: tail.to_string(),
                        ty,
                    });
                }
            }
            ExprKind::Self_ { local_id } => {
                if let Some(key) = self.local_key(*local_id) {
                    let ty = expr
                        .resolution
                        .is_resolved()
                        .then(|| expr.resolution.clone());
                    self.index.push(Occurrence {
                        key,
                        span: expr.span,
                        role: Role::Read,
                        name: "self".to_string(),
                        ty,
                    });
                }
            }
            ExprKind::MethodCall { method, target, .. } => {
                if let Resolution::Global(id) = target
                    && let Some(id) = self.canonical_global(*id)
                    && self
                        .registry
                        .get(id)
                        .is_some_and(|entry| matches!(entry.kind, GlobalKind::Function(_)))
                {
                    self.record(SymbolKey::Global(id), method, Role::Read, None);
                }
            }
            ExprKind::NamedFunctionReference { path, target, .. } => {
                if let Some(key) = self.key_for(*target)
                    && let Some(name) = path.last()
                {
                    self.record(key, name, Role::Read, None);
                }
            }
            ExprKind::StructConstruction { type_path, .. }
            | ExprKind::EnumConstruction { type_path, .. } => {
                self.record_construction(type_path, expr);
            }
            _ => {}
        }
        visit::walk_expr(self, expr);
    }

    fn visit_pattern(&mut self, pattern: &'ast Pattern) {
        match pattern {
            Pattern::Binding { local_id, name, .. } => {
                self.record_local_binding(*local_id, name, None);
            }
            Pattern::TypedBinding {
                local_id,
                name,
                resolved_type,
                ..
            } => {
                self.record_local_binding(*local_id, name, resolved_type.clone());
            }
            Pattern::EnumUnit {
                type_path,
                type_resolution,
                ..
            }
            | Pattern::EnumTuple {
                type_path,
                type_resolution,
                ..
            }
            | Pattern::EnumStruct {
                type_path,
                type_resolution,
                ..
            }
            | Pattern::Struct {
                type_path,
                type_resolution,
                ..
            } => self.record_type_path(type_path, *type_resolution),
            _ => {}
        }
        visit::walk_pattern(self, pattern);
    }

    fn visit_type_expr(&mut self, type_expr: &'ast TypeExpr) {
        if let TypeExpr::Named {
            path, resolution, ..
        }
        | TypeExpr::Generic {
            path, resolution, ..
        } = type_expr
        {
            self.record_type_path(path, *resolution);
        }
        visit::walk_type_expr(self, type_expr);
    }
}

impl Builder<'_> {
    /// `type X = ...` inside an `impl` or `extend` registers as a
    /// member of the target type.
    fn record_member_aliases(&mut self, members: &[ImplMember]) {
        let Some(owner) = self.owner.clone() else {
            return;
        };
        for member in members {
            if let ImplMember::TypeAlias(alias) = member {
                let identifier =
                    Identifier::member(owner.package(), owner.path(), alias.name.as_str());
                if let Some(id) = self.lookup(&identifier) {
                    self.record_global_declaration(id, &alias.name);
                }
            }
        }
    }
}
