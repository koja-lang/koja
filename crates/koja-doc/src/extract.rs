//! Walk the parsed AST and extract documentation items into a
//! package-aware [`DocProject`].
//!
//! A `DocProject` is a roster of [`DocPackage`]s sorted with the
//! user's own package first, then path dependencies, then stdlib,
//! alphabetical within each tier. Every doc item lives under
//! exactly one package with no cross-package flattening, so the
//! renderer can emit a clean `doc/<Pkg>/<Item>.html` tree and the
//! sidebar dropdown can pivot between packages without ambiguity.

use koja_ast::ast::{
    AnnotationKind, AnnotationValue, BuiltinDecl, EnumDecl, Expr, ExprKind, ExtendBlock, File,
    Function, ImplBlock, ImplMember, Item, Literal, Name, Param, ProtocolDecl, ProtocolMethod,
    StringPart, StructDecl, TypeExpr, TypeParam, UnaryOp, Visibility, name_texts, path_text,
};
use koja_ast::util::dedent;

/// Where a [`DocPackage`] came from. Drives the cross-package sort
/// order (project -> dependency -> stdlib, alphabetical within tier)
/// and lets the renderer label package origins in the roster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind {
    Project,
    Dependency,
    Stdlib,
}

impl PackageKind {
    /// Tier ordinal for the package sort, where lower comes first.
    fn tier(self) -> u8 {
        match self {
            PackageKind::Project => 0,
            PackageKind::Dependency => 1,
            PackageKind::Stdlib => 2,
        }
    }

    /// Short label shown next to a package name in the roster page.
    pub fn label(self) -> &'static str {
        match self {
            PackageKind::Project => "project",
            PackageKind::Dependency => "dependency",
            PackageKind::Stdlib => "stdlib",
        }
    }
}

/// Summary of a documentable item for the flat index listing.
#[derive(Debug)]
pub struct DocItem {
    pub deprecated: Option<String>,
    pub doc: Option<String>,
    pub kind: String,
    pub href: String,
    pub name: String,
}

/// One protocol a type conforms to, from its header (`struct P: Hash`)
/// or from an `impl Protocol for Type` block. `protocol` is the display
/// text with generic arguments, qualified with the protocol's package
/// once it resolves (`Global.Enumeration<T, Int>`). `condition`
/// is the rendered bound list of a conditional impl (`T: Equality`).
/// `protocol_href` is root-relative (`Global/Hash.html`) and set only
/// when the protocol is documented. `functions` holds the requirement
/// implementations, moved here from the type's inherent list for a
/// header conformance. `impl_package` is the package that declared
/// an `impl` block, where its protocol path resolves. A header
/// conformance resolves from the type's own package and leaves it
/// `None`.
#[derive(Debug)]
pub struct DocConformance {
    pub condition: Option<String>,
    pub functions: Vec<DocFunction>,
    pub impl_package: Option<String>,
    pub protocol: String,
    pub protocol_href: Option<String>,
    pub protocol_path: Vec<String>,
}

/// One type that conforms to a protocol, listed on the protocol page.
/// `href` is root-relative (`Global/Int.html`).
#[derive(Debug)]
pub struct DocImplementor {
    pub condition: Option<String>,
    pub doc: Option<String>,
    pub href: String,
    pub kind: String,
    pub package: String,
    pub type_name: String,
}

/// Documentation for a constant.
#[derive(Debug)]
pub struct DocConstant {
    pub deprecated: Option<String>,
    pub doc: Option<String>,
    pub name: String,
}

/// Documentation for an enum.
#[derive(Debug)]
pub struct DocEnum {
    pub conformances: Vec<DocConformance>,
    pub deprecated: Option<String>,
    pub doc: Option<String>,
    pub functions: Vec<DocFunction>,
    pub name: String,
    pub variants: Vec<String>,
}

/// A struct field for display. `default` is the rendered default
/// value when the field declares one.
#[derive(Debug)]
pub struct DocField {
    pub default: Option<String>,
    pub name: String,
    pub type_name: String,
}

/// Documentation for a function. `error_type` is `Some` for the
/// fallible spelling `-> T ! E`.
#[derive(Debug)]
pub struct DocFunction {
    pub arity: usize,
    pub deprecated: Option<String>,
    pub doc: Option<String>,
    pub error_type: Option<String>,
    pub name: String,
    pub params: Vec<DocParam>,
    pub return_type: Option<String>,
    pub type_params: Vec<String>,
}

/// A function parameter for display.
#[derive(Debug)]
pub struct DocParam {
    pub name: String,
    pub type_name: String,
}

/// Documentation for a protocol. `implementors` is filled in
/// [`finalize_project`] from every documented type that conforms.
#[derive(Debug)]
pub struct DocProtocol {
    pub deprecated: Option<String>,
    pub doc: Option<String>,
    pub functions: Vec<DocFunction>,
    pub implementors: Vec<DocImplementor>,
    pub name: String,
    pub type_params: Vec<String>,
}

/// Documentation for a builtin type. Builtins carry functions but no
/// fields, the compiler owns their representation.
#[derive(Debug)]
pub struct DocBuiltin {
    pub conformances: Vec<DocConformance>,
    pub deprecated: Option<String>,
    pub doc: Option<String>,
    pub functions: Vec<DocFunction>,
    pub name: String,
    pub type_params: Vec<String>,
}

/// Documentation for a struct, including its impl functions.
#[derive(Debug)]
pub struct DocStruct {
    pub conformances: Vec<DocConformance>,
    pub deprecated: Option<String>,
    pub doc: Option<String>,
    pub fields: Vec<DocField>,
    pub functions: Vec<DocFunction>,
    pub name: String,
    pub type_params: Vec<String>,
}

/// All extracted documentation for a single package. Every kind of
/// item lives here, plus a flat [`Self::items`] roster used by the
/// sidebar item list. `kind` is the origin tier (project / dep /
/// stdlib) and drives cross-package sort + renderer labelling.
///
/// `pending_extends` and `pending_impls` hold methods from
/// `extend Type` and `impl Protocol for Type` blocks declared in this
/// package that haven't yet been routed to their target type.
/// [`finalize_project`] drains them once every file has been
/// ingested, so same-package and cross-package targets route
/// identically.
#[derive(Debug)]
pub struct DocPackage {
    pub builtins: Vec<DocBuiltin>,
    pub constants: Vec<DocConstant>,
    pub enums: Vec<DocEnum>,
    pub functions: Vec<DocFunction>,
    pub items: Vec<DocItem>,
    pub kind: PackageKind,
    pub name: String,
    pub protocols: Vec<DocProtocol>,
    pub structs: Vec<DocStruct>,
    pending_extends: Vec<PendingExtend>,
    pending_impls: Vec<PendingImpl>,
}

/// A method-set from an `extend Type` block, consumed by
/// [`resolve_pending_extends`] before rendering.
#[derive(Debug)]
struct PendingExtend {
    current_package: String,
    functions: Vec<DocFunction>,
    target_path: Vec<String>,
}

/// An `impl Protocol for Type` block, consumed by
/// [`resolve_pending_impls`] before rendering. The conformance is
/// built here so the target only has to adopt it.
#[derive(Debug)]
struct PendingImpl {
    conformance: DocConformance,
    current_package: String,
    target_path: Vec<String>,
}

impl DocPackage {
    fn new(name: String, kind: PackageKind) -> Self {
        Self {
            builtins: Vec::new(),
            constants: Vec::new(),
            enums: Vec::new(),
            functions: Vec::new(),
            items: Vec::new(),
            kind,
            name,
            protocols: Vec::new(),
            structs: Vec::new(),
            pending_extends: Vec::new(),
            pending_impls: Vec::new(),
        }
    }
}

/// Documentation for an entire project, the user's own package
/// (named in `project_package`) plus any deps and stdlib packages
/// the driver chose to bundle in. The renderer walks
/// [`Self::packages`] to emit one subdir per package.
#[derive(Debug)]
pub struct DocProject {
    /// Bare name of the user's own package, used as the default
    /// landing page and to highlight the project entry in the
    /// sidebar dropdown. May be empty when running in loose-file
    /// mode with no `koja.toml`.
    pub project_package: String,
    pub packages: Vec<DocPackage>,
}

impl DocProject {
    /// Construct an empty project that the driver fills in by
    /// repeatedly calling [`extract_items`] for each source file.
    pub fn new(project_package: impl Into<String>) -> Self {
        Self {
            project_package: project_package.into(),
            packages: Vec::new(),
        }
    }

    /// Find-or-create the [`DocPackage`] for `name`. New packages
    /// adopt the supplied `kind`. If the package already exists
    /// the existing kind is preserved (first caller wins).
    pub fn ensure_package(&mut self, name: &str, kind: PackageKind) -> &mut DocPackage {
        if let Some(idx) = self.packages.iter().position(|p| p.name == name) {
            return &mut self.packages[idx];
        }
        self.packages.push(DocPackage::new(name.to_string(), kind));
        self.packages.last_mut().expect("just pushed a package")
    }

    /// Find a package by name. Used by the renderer when looking
    /// up a cross-package type reference.
    pub fn find_package(&self, name: &str) -> Option<&DocPackage> {
        self.packages.iter().find(|p| p.name == name)
    }
}

/// Extract documentation items from a parsed file into `package`
/// inside `project`. Items with `@doc false` and `priv` declarations
/// are excluded. `extend Type` and `impl Protocol for Type` blocks
/// queue their methods on the current package's pending lists for
/// [`finalize_project`] to distribute to the target type.
pub fn extract_items(file: &File, project: &mut DocProject, package: &str, kind: PackageKind) {
    let pkg = project.ensure_package(package, kind);

    for item in &file.items {
        match item {
            Item::Alias(_) | Item::Test(_) => {}
            Item::Builtin(b) => {
                if let Some(db) = extract_builtin(b) {
                    pkg.builtins.push(db);
                }
                for nested in &b.nested {
                    extract_type_item(nested, pkg, &name_texts(&b.path));
                }
            }
            Item::Constant(_) => {
                extract_type_item(item, pkg, &[]);
            }
            Item::Enum(_) => {
                extract_type_item(item, pkg, &[]);
            }
            Item::Extend(ext) => {
                if let Some(pending) = make_pending_extend(ext, package) {
                    pkg.pending_extends.push(pending);
                }
            }
            Item::Function(f) => {
                if let Some(df) = extract_function(f) {
                    pkg.functions.push(df);
                }
            }
            Item::Impl(block) => {
                if let Some(pending) = make_pending_impl(block, package) {
                    pkg.pending_impls.push(pending);
                }
            }
            Item::Protocol(_) => {
                extract_type_item(item, pkg, &[]);
            }
            Item::Struct(_) => {
                extract_type_item(item, pkg, &[]);
            }
            Item::TypeAlias(_) => {}
        }
    }
}

/// Extract a struct, enum, protocol, or constant under its full owner
/// path, and recursively flatten a struct's or enum's lexical nested
/// items. Private owners hide their subtree.
fn extract_type_item(item: &Item, pkg: &mut DocPackage, owner_path: &[String]) {
    match item {
        Item::Enum(decl) => {
            if decl.visibility == Visibility::Private {
                return;
            }
            let path = nested_path(owner_path, &decl.path);
            if let Some(extracted) = extract_enum(decl, &path) {
                pkg.enums.push(extracted);
            }
            for nested in &decl.nested {
                extract_type_item(nested, pkg, &path);
            }
        }
        Item::Struct(decl) => {
            if decl.visibility == Visibility::Private {
                return;
            }
            let path = nested_path(owner_path, &decl.path);
            if let Some(extracted) = extract_struct(decl, &path) {
                pkg.structs.push(extracted);
            }
            for nested in &decl.nested {
                extract_type_item(nested, pkg, &path);
            }
        }
        Item::Protocol(decl) => {
            let path = nested_path(owner_path, &decl.path);
            if let Some(extracted) = extract_protocol(decl, &path) {
                pkg.protocols.push(extracted);
            }
        }
        Item::Constant(decl) => {
            let path = nested_path(owner_path, &decl.path);
            if let Some(extracted) = extract_constant(decl, &path) {
                pkg.constants.push(extracted);
            }
        }
        _ => debug_assert!(
            false,
            "nested declarations are structs, enums, protocols, or constants"
        ),
    }
}

fn nested_path(owner_path: &[String], path: &[Name]) -> Vec<String> {
    owner_path.iter().cloned().chain(name_texts(path)).collect()
}

/// Resolve pending `extend` and `impl` blocks, link conformances to
/// their protocols, sort packages by `(kind tier, name)` so the
/// user's project lands first, then sort and flatten each package's
/// items for the sidebar.
pub fn finalize_project(project: &mut DocProject) {
    resolve_pending_extends(project);
    resolve_pending_impls(project);
    link_conformances(project);

    project
        .packages
        .sort_by(|a, b| a.kind.tier().cmp(&b.kind.tier()).then(a.name.cmp(&b.name)));

    for pkg in &mut project.packages {
        finalize_package(pkg);
    }
}

/// Drain every package's `pending_extends` and attach each method
/// set to the named struct or enum. Extends whose target isn't
/// documented (private type, unbundled package) are dropped.
fn resolve_pending_extends(project: &mut DocProject) {
    let pendings: Vec<PendingExtend> = project
        .packages
        .iter_mut()
        .flat_map(|pkg| std::mem::take(&mut pkg.pending_extends))
        .collect();

    for pending in pendings {
        let Some((package_idx, target_name)) = resolve_target(
            project,
            &pending.current_package,
            &pending.target_path,
            package_has_type,
        ) else {
            continue;
        };
        let target = &mut project.packages[package_idx];
        if let Some(db) = target.builtins.iter_mut().find(|b| b.name == target_name) {
            db.functions.extend(pending.functions);
        } else if let Some(ds) = target.structs.iter_mut().find(|s| s.name == target_name) {
            ds.functions.extend(pending.functions);
        } else if let Some(de) = target.enums.iter_mut().find(|e| e.name == target_name) {
            de.functions.extend(pending.functions);
        }
    }
}

/// Drain every package's `pending_impls` and attach each one to the
/// named type as a conformance. Impls whose target isn't documented
/// are dropped, same as extends.
fn resolve_pending_impls(project: &mut DocProject) {
    let pendings: Vec<PendingImpl> = project
        .packages
        .iter_mut()
        .flat_map(|pkg| std::mem::take(&mut pkg.pending_impls))
        .collect();

    for pending in pendings {
        let Some((package_idx, target_name)) = resolve_target(
            project,
            &pending.current_package,
            &pending.target_path,
            package_has_type,
        ) else {
            continue;
        };
        let target = &mut project.packages[package_idx];
        if let Some(db) = target.builtins.iter_mut().find(|b| b.name == target_name) {
            db.conformances.push(pending.conformance);
        } else if let Some(ds) = target.structs.iter_mut().find(|s| s.name == target_name) {
            ds.conformances.push(pending.conformance);
        } else if let Some(de) = target.enums.iter_mut().find(|e| e.name == target_name) {
            de.conformances.push(pending.conformance);
        }
    }
}

#[derive(Clone, Copy)]
enum TypeKind {
    Builtin,
    Enum,
    Struct,
}

impl TypeKind {
    /// The kind chip text, same as [`DocItem::kind`].
    fn label(self) -> &'static str {
        match self {
            TypeKind::Builtin => "builtin",
            TypeKind::Enum => "enum",
            TypeKind::Struct => "struct",
        }
    }
}

/// One conformance that resolved to a documented protocol. Indices
/// point into `project.packages` and stay valid until
/// [`finalize_package`] sorts, which runs after linking.
struct Link {
    conformance_idx: usize,
    kind: TypeKind,
    package_idx: usize,
    protocol: ProtocolRef,
    type_idx: usize,
}

/// What a linked conformance needs from its protocol, copied out so
/// the type can be edited while nothing else borrows the project.
struct ProtocolRef {
    name: String,
    package: String,
    package_idx: usize,
    requirements: Vec<Requirement>,
}

struct Requirement {
    arity: usize,
    doc: Option<String>,
    name: String,
}

/// Connect every conformance to its protocol. A found protocol gets
/// its href set, a header conformance takes the type's functions that
/// match a requirement name, undocumented conformance functions
/// inherit the requirement's `@doc`, and the protocol gains an
/// implementor. A protocol that isn't documented leaves the
/// conformance as a name only.
fn link_conformances(project: &mut DocProject) {
    let mut links = Vec::new();
    for (package_idx, pkg) in project.packages.iter().enumerate() {
        let mut collect = |kind: TypeKind, type_idx: usize, conformances: &[DocConformance]| {
            for (conformance_idx, c) in conformances.iter().enumerate() {
                let from = c.impl_package.as_deref().unwrap_or(&pkg.name);
                let Some(protocol) = protocol_ref(project, from, &c.protocol_path) else {
                    continue;
                };
                links.push(Link {
                    conformance_idx,
                    kind,
                    package_idx,
                    protocol,
                    type_idx,
                });
            }
        };
        for (i, b) in pkg.builtins.iter().enumerate() {
            collect(TypeKind::Builtin, i, &b.conformances);
        }
        for (i, e) in pkg.enums.iter().enumerate() {
            collect(TypeKind::Enum, i, &e.conformances);
        }
        for (i, s) in pkg.structs.iter().enumerate() {
            collect(TypeKind::Struct, i, &s.conformances);
        }
    }

    let mut implementors: Vec<(&ProtocolRef, DocImplementor)> = Vec::new();
    for link in &links {
        let pkg = &mut project.packages[link.package_idx];
        let owner_package = pkg.name.clone();
        let (owner_name, owner_doc, conformances, functions) = match link.kind {
            TypeKind::Builtin => {
                let b = &mut pkg.builtins[link.type_idx];
                (
                    b.name.clone(),
                    b.doc.clone(),
                    &mut b.conformances,
                    &mut b.functions,
                )
            }
            TypeKind::Enum => {
                let e = &mut pkg.enums[link.type_idx];
                (
                    e.name.clone(),
                    e.doc.clone(),
                    &mut e.conformances,
                    &mut e.functions,
                )
            }
            TypeKind::Struct => {
                let s = &mut pkg.structs[link.type_idx];
                (
                    s.name.clone(),
                    s.doc.clone(),
                    &mut s.conformances,
                    &mut s.functions,
                )
            }
        };
        let conformance = &mut conformances[link.conformance_idx];
        link_conformance(conformance, functions, &link.protocol);
        implementors.push((
            &link.protocol,
            DocImplementor {
                condition: conformance.condition.clone(),
                doc: owner_doc,
                href: format!("{owner_package}/{owner_name}.html"),
                kind: link.kind.label().to_string(),
                package: owner_package,
                type_name: owner_name,
            },
        ));
    }

    for (protocol, implementor) in implementors {
        let pkg = &mut project.packages[protocol.package_idx];
        if let Some(p) = pkg.protocols.iter_mut().find(|p| p.name == protocol.name) {
            p.implementors.push(implementor);
        }
    }
}

/// Look a conformance's protocol up by the same rule as an extend
/// target and copy out what linking needs.
fn protocol_ref(
    project: &DocProject,
    current_package: &str,
    path: &[String],
) -> Option<ProtocolRef> {
    let (package_idx, name) = resolve_target(project, current_package, path, package_has_protocol)?;
    let pkg = &project.packages[package_idx];
    let protocol = pkg.protocols.iter().find(|p| p.name == name)?;
    Some(ProtocolRef {
        name,
        package: pkg.name.clone(),
        package_idx,
        requirements: protocol
            .functions
            .iter()
            .map(|f| Requirement {
                arity: f.arity,
                doc: f.doc.clone(),
                name: f.name.clone(),
            })
            .collect(),
    })
}

/// Set the href, qualify the protocol with its package, move header
/// functions under the conformance, and fill missing docs from the
/// protocol requirements. A conformance that arrived with functions
/// came from an `impl` block and keeps them as they are.
fn link_conformance(
    conformance: &mut DocConformance,
    functions: &mut Vec<DocFunction>,
    protocol: &ProtocolRef,
) {
    conformance.protocol_href = Some(format!("{}/{}.html", protocol.package, protocol.name));

    // The display text starts with the source path. Swap that head
    // for the resolved `Pkg.Name` and keep any generic arguments.
    let source_head = conformance.protocol_path.join(".");
    let args = conformance
        .protocol
        .strip_prefix(source_head.as_str())
        .unwrap_or_default();
    conformance.protocol = format!("{}.{}{args}", protocol.package, protocol.name);
    conformance.protocol_path = std::iter::once(protocol.package.clone())
        .chain(protocol.name.split('.').map(str::to_string))
        .collect();

    if conformance.functions.is_empty() {
        let (moved, kept): (Vec<_>, Vec<_>) = std::mem::take(functions)
            .into_iter()
            .partition(|f| protocol.requirements.iter().any(|r| r.name == f.name));
        *functions = kept;
        conformance.functions = moved;
    }

    for f in &mut conformance.functions {
        if f.doc.is_some() {
            continue;
        }
        f.doc = protocol
            .requirements
            .iter()
            .find(|r| r.name == f.name && r.arity == f.arity)
            .and_then(|r| r.doc.clone());
    }
}

/// The prelude package. Bare names that miss in the current package
/// fall back here, as in typecheck's `lookup_owner_path`.
const PRELUDE_PACKAGE: &str = "Global";

/// Resolve a type path like typecheck does. Prefer the complete path
/// in the current package, then read the first path segment as a
/// package, then try the prelude. `has` decides which item kinds
/// count as a match.
fn resolve_target(
    project: &DocProject,
    current_package: &str,
    target_path: &[String],
    has: fn(&DocPackage, &str) -> bool,
) -> Option<(usize, String)> {
    let find = |package_name: &str, name: &str| {
        project
            .packages
            .iter()
            .position(|package| package.name == package_name && has(package, name))
            .map(|package_idx| (package_idx, name.to_string()))
    };

    let local_name = target_path.join(".");
    if let Some(found) = find(current_package, &local_name) {
        return Some(found);
    }
    if let [package_name, rest @ ..] = target_path
        && !rest.is_empty()
        && let Some(found) = find(package_name, &rest.join("."))
    {
        return Some(found);
    }
    find(PRELUDE_PACKAGE, &local_name)
}

fn package_has_type(package: &DocPackage, name: &str) -> bool {
    package.builtins.iter().any(|item| item.name == name)
        || package.enums.iter().any(|item| item.name == name)
        || package.structs.iter().any(|item| item.name == name)
}

fn package_has_protocol(package: &DocPackage, name: &str) -> bool {
    package.protocols.iter().any(|item| item.name == name)
}

fn sort_functions(functions: &mut [DocFunction]) {
    functions.sort_by(|a, b| (&a.name, a.arity).cmp(&(&b.name, b.arity)));
}

fn sort_conformances(conformances: &mut [DocConformance]) {
    conformances.sort_by(|a, b| a.protocol.cmp(&b.protocol));
    for c in conformances {
        sort_functions(&mut c.functions);
    }
}

fn finalize_package(pkg: &mut DocPackage) {
    pkg.builtins.sort_by(|a, b| a.name.cmp(&b.name));
    pkg.constants.sort_by(|a, b| a.name.cmp(&b.name));
    pkg.enums.sort_by(|a, b| a.name.cmp(&b.name));
    sort_functions(&mut pkg.functions);
    pkg.protocols.sort_by(|a, b| a.name.cmp(&b.name));
    pkg.structs.sort_by(|a, b| a.name.cmp(&b.name));

    for b in &mut pkg.builtins {
        sort_functions(&mut b.functions);
        sort_conformances(&mut b.conformances);
    }
    for e in &mut pkg.enums {
        sort_functions(&mut e.functions);
        sort_conformances(&mut e.conformances);
    }
    for p in &mut pkg.protocols {
        sort_functions(&mut p.functions);
        p.implementors
            .sort_by(|a, b| (&a.package, &a.type_name).cmp(&(&b.package, &b.type_name)));
    }
    for s in &mut pkg.structs {
        sort_functions(&mut s.functions);
        sort_conformances(&mut s.conformances);
    }

    pkg.items.clear();
    for b in &pkg.builtins {
        pkg.items.push(DocItem {
            deprecated: b.deprecated.clone(),
            doc: b.doc.clone(),
            kind: "builtin".to_string(),
            href: b.name.clone(),
            name: b.name.clone(),
        });
    }
    for c in &pkg.constants {
        pkg.items.push(DocItem {
            deprecated: c.deprecated.clone(),
            doc: c.doc.clone(),
            kind: "const".to_string(),
            href: c.name.clone(),
            name: c.name.clone(),
        });
    }
    for e in &pkg.enums {
        pkg.items.push(DocItem {
            deprecated: e.deprecated.clone(),
            doc: e.doc.clone(),
            kind: "enum".to_string(),
            href: e.name.clone(),
            name: e.name.clone(),
        });
    }
    for f in &pkg.functions {
        pkg.items.push(DocItem {
            deprecated: f.deprecated.clone(),
            doc: f.doc.clone(),
            kind: "fn".to_string(),
            href: f.page_name(),
            name: f.display_name(),
        });
    }
    for p in &pkg.protocols {
        pkg.items.push(DocItem {
            deprecated: p.deprecated.clone(),
            doc: p.doc.clone(),
            kind: "protocol".to_string(),
            href: p.name.clone(),
            name: p.name.clone(),
        });
    }
    for s in &pkg.structs {
        pkg.items.push(DocItem {
            deprecated: s.deprecated.clone(),
            doc: s.doc.clone(),
            kind: "struct".to_string(),
            href: s.name.clone(),
            name: s.name.clone(),
        });
    }
    pkg.items.sort_by(|a, b| a.name.cmp(&b.name));
}

/// Read the `@doc` string, dedented so the declaration's source
/// indentation doesn't leak into markdown or terminal rendering.
fn annotation_string(annotations: &[koja_ast::ast::Annotation]) -> Option<String> {
    annotations
        .iter()
        .find(|a| a.name == "doc")
        .and_then(|a| match &a.value {
            Some(AnnotationValue::String(s)) => Some(dedent(s).trim().to_string()),
            _ => None,
        })
}

fn annotation_deprecated(annotations: &[koja_ast::ast::Annotation]) -> Option<String> {
    annotations.iter().find_map(|annotation| {
        let AnnotationKind::Deprecated { message } = annotation.kind() else {
            return None;
        };
        let message = dedent(message).trim().to_string();
        (!message.is_empty()).then_some(message)
    })
}

/// Build a [`PendingExtend`] from an `extend Type` block. Path
/// interpretation mirrors typecheck/IR's `extend_target_path`,
/// inlined so `koja-doc` doesn't need a typecheck dep.
fn make_pending_extend(ext: &ExtendBlock, current_package: &str) -> Option<PendingExtend> {
    let path = match &ext.target {
        TypeExpr::Generic { path, .. } | TypeExpr::Named { path, .. } => path,
        _ => return None,
    };
    if path.is_empty() {
        return None;
    }

    let functions: Vec<DocFunction> = ext
        .members
        .iter()
        .filter_map(|m| match m {
            ImplMember::Function(f) => extract_function(f),
            ImplMember::TypeAlias(_) => None,
        })
        .collect();

    if functions.is_empty() {
        return None;
    }

    Some(PendingExtend {
        current_package: current_package.to_string(),
        functions,
        target_path: name_texts(path),
    })
}

/// Build a [`PendingImpl`] from an `impl Protocol for Type` block.
/// The target path follows the same rule as `extend`. An impl with
/// no documentable members still records the conformance.
fn make_pending_impl(block: &ImplBlock, current_package: &str) -> Option<PendingImpl> {
    let target_path = match &block.target {
        TypeExpr::Generic { path, .. } | TypeExpr::Named { path, .. } => name_texts(path),
        _ => return None,
    };
    if target_path.is_empty() {
        return None;
    }
    let mut conformance = conformance_from_type_expr(&block.trait_expr)?;
    conformance.condition = bounds_to_string(&block.target_bounds);
    conformance.impl_package = Some(current_package.to_string());
    conformance.functions = block
        .members
        .iter()
        .filter_map(|m| match m {
            ImplMember::Function(f) => extract_function(f),
            ImplMember::TypeAlias(_) => None,
        })
        .collect();

    Some(PendingImpl {
        conformance,
        current_package: current_package.to_string(),
        target_path,
    })
}

/// Read a header conformance list (`struct P: Hash, Display`) into
/// conformances with no functions yet. [`link_conformances`] moves the
/// requirement implementations under each one later.
fn header_conformances(conformances: &[TypeExpr]) -> Vec<DocConformance> {
    conformances
        .iter()
        .filter_map(conformance_from_type_expr)
        .collect()
}

fn conformance_from_type_expr(ty: &TypeExpr) -> Option<DocConformance> {
    let path = match ty {
        TypeExpr::Generic { path, .. } | TypeExpr::Named { path, .. } => path,
        _ => return None,
    };
    Some(DocConformance {
        condition: None,
        functions: Vec::new(),
        impl_package: None,
        protocol: type_expr_to_string(ty),
        protocol_href: None,
        protocol_path: name_texts(path),
    })
}

/// Render the bound list of a conditional impl, `T: Equality` or
/// `K: Hash & Debug, V: Debug`. Unbounded parameters are left out.
/// `None` when nothing remains.
fn bounds_to_string(params: &[TypeParam]) -> Option<String> {
    let parts: Vec<String> = params
        .iter()
        .filter(|tp| !tp.bounds.is_empty())
        .map(|tp| {
            let bounds: Vec<String> = tp.bounds.iter().map(type_expr_to_string).collect();
            format!("{}: {}", tp.name.text, bounds.join(" & "))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn extract_constant(c: &koja_ast::ast::Constant, path: &[String]) -> Option<DocConstant> {
    if c.visibility == Visibility::Private || has_doc_false(&c.annotations) {
        return None;
    }

    Some(DocConstant {
        deprecated: annotation_deprecated(&c.annotations),
        doc: annotation_string(&c.annotations),
        name: path.join("."),
    })
}

fn extract_enum(e: &EnumDecl, path: &[String]) -> Option<DocEnum> {
    if e.visibility == Visibility::Private || has_doc_false(&e.annotations) {
        return None;
    }

    let variants = e.variants.iter().map(|v| v.name.text.clone()).collect();
    let functions = e.functions.iter().filter_map(extract_function).collect();

    Some(DocEnum {
        conformances: header_conformances(&e.conformances),
        deprecated: annotation_deprecated(&e.annotations),
        doc: annotation_string(&e.annotations),
        functions,
        name: path.join("."),
        variants,
    })
}

fn extract_function(f: &Function) -> Option<DocFunction> {
    if matches!(
        f.origin,
        koja_ast::ast::FunctionOrigin::DefaultAdapter { .. } | koja_ast::ast::FunctionOrigin::Test
    ) || f.visibility == Visibility::Private
        || has_doc_false(&f.annotations)
    {
        return None;
    }

    let params = extract_params(&f.params);

    Some(DocFunction {
        arity: f.params.len(),
        deprecated: annotation_deprecated(&f.annotations),
        doc: annotation_string(&f.annotations),
        error_type: f.error_type.as_ref().map(type_expr_to_string),
        name: f.name.text.clone(),
        params,
        return_type: f.return_type.as_ref().map(type_expr_to_string),
        type_params: f
            .type_params
            .iter()
            .map(|tp| tp.name.text.clone())
            .collect(),
    })
}

fn extract_params(params: &[Param]) -> Vec<DocParam> {
    params
        .iter()
        .map(|p| match p {
            Param::Self_ { .. } => DocParam {
                name: "self".to_string(),
                type_name: String::new(),
            },
            Param::Regular {
                name, type_expr, ..
            } => DocParam {
                name: name.text.clone(),
                type_name: type_expr_to_string(type_expr),
            },
        })
        .collect()
}

fn extract_protocol(p: &ProtocolDecl, path: &[String]) -> Option<DocProtocol> {
    if p.visibility == Visibility::Private || has_doc_false(&p.annotations) {
        return None;
    }

    let functions = p
        .methods
        .iter()
        .filter_map(extract_protocol_method)
        .collect();

    Some(DocProtocol {
        deprecated: annotation_deprecated(&p.annotations),
        doc: annotation_string(&p.annotations),
        functions,
        implementors: Vec::new(),
        name: path.join("."),
        type_params: p
            .type_params
            .iter()
            .map(|tp| tp.name.text.clone())
            .collect(),
    })
}

fn extract_protocol_method(m: &ProtocolMethod) -> Option<DocFunction> {
    if matches!(
        m.origin,
        koja_ast::ast::FunctionOrigin::DefaultAdapter { .. }
    ) || has_doc_false(&m.annotations)
    {
        return None;
    }

    let params = extract_params(&m.params);

    Some(DocFunction {
        arity: m.params.len(),
        deprecated: annotation_deprecated(&m.annotations),
        doc: annotation_string(&m.annotations),
        error_type: m.error_type.as_ref().map(type_expr_to_string),
        name: m.name.text.clone(),
        params,
        return_type: m.return_type.as_ref().map(type_expr_to_string),
        type_params: m
            .type_params
            .iter()
            .map(|tp| tp.name.text.clone())
            .collect(),
    })
}

fn extract_struct(s: &StructDecl, path: &[String]) -> Option<DocStruct> {
    if s.visibility == Visibility::Private || has_doc_false(&s.annotations) {
        return None;
    }

    let fields = s
        .fields
        .iter()
        .map(|f| DocField {
            default: f.default.as_ref().map(default_to_string),
            name: f.name.text.clone(),
            type_name: type_expr_to_string(&f.type_expr),
        })
        .collect();
    let functions = s.functions.iter().filter_map(extract_function).collect();

    Some(DocStruct {
        conformances: header_conformances(&s.conformances),
        deprecated: annotation_deprecated(&s.annotations),
        doc: annotation_string(&s.annotations),
        fields,
        functions,
        name: path.join("."),
        type_params: s
            .type_params
            .iter()
            .map(|tp| tp.name.text.clone())
            .collect(),
    })
}

fn extract_builtin(b: &BuiltinDecl) -> Option<DocBuiltin> {
    if has_doc_false(&b.annotations) {
        return None;
    }
    Some(DocBuiltin {
        conformances: Vec::new(),
        deprecated: annotation_deprecated(&b.annotations),
        doc: annotation_string(&b.annotations),
        functions: b.functions.iter().filter_map(extract_function).collect(),
        name: b.name().to_string(),
        type_params: b
            .type_params
            .iter()
            .map(|tp| tp.name.text.clone())
            .collect(),
    })
}

fn has_doc_false(annotations: &[koja_ast::ast::Annotation]) -> bool {
    annotations
        .iter()
        .any(|a| a.name == "doc" && a.value == Some(AnnotationValue::False))
}

/// Format a default-value expression for display. Covers the shapes
/// the compiler accepts as field defaults: literals, negated
/// numerics, unit enum variants, binary literals, and struct, list,
/// map, or set literals of those.
fn default_to_string(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::BinaryLiteral { segments } => {
            let parts: Vec<String> = segments
                .iter()
                .map(|segment| match segment.size.as_deref() {
                    Some(size) => {
                        format!(
                            "{}::{}",
                            default_to_string(&segment.value),
                            default_to_string(size)
                        )
                    }
                    None => default_to_string(&segment.value),
                })
                .collect();
            format!("<<{}>>", parts.join(", "))
        }
        ExprKind::EnumConstruction {
            type_path, variant, ..
        } => format!("{}.{variant}", path_text(type_path)),
        ExprKind::Group { expr: inner } => format!("({})", default_to_string(inner)),
        ExprKind::List { elements } => {
            let parts: Vec<String> = elements.iter().map(default_to_string).collect();
            format!("[{}]", parts.join(", "))
        }
        ExprKind::Literal { value } => match value {
            Literal::Bool(b) => b.to_string(),
            Literal::Float(text) | Literal::Int(text) => text.clone(),
            Literal::String(text) => format!("\"{text}\""),
            Literal::Unit => "()".to_string(),
        },
        ExprKind::Map { entries } => {
            if entries.is_empty() {
                return "[:]".to_string();
            }
            let parts: Vec<String> = entries
                .iter()
                .map(|(key, value)| {
                    format!("{}: {}", default_to_string(key), default_to_string(value))
                })
                .collect();
            format!("[{}]", parts.join(", "))
        }
        ExprKind::String { parts, .. } => {
            let text: String = parts
                .iter()
                .filter_map(|part| match part {
                    StringPart::Literal { value, .. } => Some(value.as_str()),
                    StringPart::Interpolation { .. } => None,
                })
                .collect();
            format!("\"{text}\"")
        }
        ExprKind::StructConstruction { type_path, fields } => {
            let parts: Vec<String> = fields
                .iter()
                .map(|field| format!("{}: {}", field.name, default_to_string(&field.value)))
                .collect();
            format!("{}{{{}}}", path_text(type_path), parts.join(", "))
        }
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => format!("-{}", default_to_string(operand)),
        _ => "…".to_string(),
    }
}

/// Format a type expression as a human-readable string.
fn type_expr_to_string(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named { path, .. } => path_text(path),
        TypeExpr::Generic { path, args, .. } => {
            let args_str: Vec<String> = args.iter().map(type_expr_to_string).collect();
            format!("{}<{}>", path_text(path), args_str.join(", "))
        }
        TypeExpr::Unit { .. } => "()".to_string(),
        TypeExpr::Self_ { .. } => "Self".to_string(),
        TypeExpr::Function {
            params,
            return_type,
            ..
        } => {
            let ps: Vec<String> = params.iter().map(type_expr_to_string).collect();
            format!(
                "fn({}) -> {}",
                ps.join(", "),
                type_expr_to_string(return_type)
            )
        }
        TypeExpr::Tuple { elements, .. } => {
            let parts: Vec<String> = elements.iter().map(type_expr_to_string).collect();
            format!("({})", parts.join(", "))
        }
        TypeExpr::Union { types, .. } => {
            let parts: Vec<String> = types.iter().map(type_expr_to_string).collect();
            parts.join(" | ")
        }
    }
}
