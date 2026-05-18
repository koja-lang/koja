//! Walk the parsed AST and extract documentation items into a
//! package-aware [`DocProject`].
//!
//! A `DocProject` is a roster of [`DocPackage`]s sorted with the
//! user's own package first, then path dependencies, then stdlib,
//! alphabetical within each tier. Every kind of doc item lives
//! under exactly one package — there's no cross-package
//! flattening — so the renderer can emit a clean
//! `doc/<Pkg>/<Item>.html` tree and the sidebar dropdown can
//! pivot between packages without ambiguity.

use expo_ast::ast::{
    AnnotationValue, EnumDecl, ExtendBlock, File, Function, ImplMember, Item, Param, ProtocolDecl,
    ProtocolMethod, StructDecl, TypeExpr, Visibility,
};

/// Where a [`DocPackage`] came from. Drives the cross-package sort
/// order (project → dependency → stdlib, alphabetical within tier)
/// and lets the renderer label package origins in the roster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind {
    Project,
    Dependency,
    Stdlib,
}

impl PackageKind {
    /// Tier ordinal for the package sort: lower comes first.
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
    pub doc: Option<String>,
    pub kind: String,
    pub name: String,
}

/// Documentation for a constant.
#[derive(Debug)]
pub struct DocConstant {
    pub doc: Option<String>,
    pub name: String,
}

/// Documentation for an enum.
#[derive(Debug)]
pub struct DocEnum {
    pub doc: Option<String>,
    pub functions: Vec<DocFunction>,
    pub name: String,
    pub variants: Vec<String>,
}

/// A struct field for display.
#[derive(Debug)]
pub struct DocField {
    pub name: String,
    pub type_name: String,
}

/// Documentation for a function.
#[derive(Debug)]
pub struct DocFunction {
    pub doc: Option<String>,
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

/// Documentation for a protocol.
#[derive(Debug)]
pub struct DocProtocol {
    pub doc: Option<String>,
    pub functions: Vec<DocFunction>,
    pub name: String,
    pub type_params: Vec<String>,
}

/// Documentation for a struct, including its impl functions.
#[derive(Debug)]
pub struct DocStruct {
    pub doc: Option<String>,
    pub fields: Vec<DocField>,
    pub functions: Vec<DocFunction>,
    pub name: String,
    pub type_params: Vec<String>,
}

/// All extracted documentation for a single package: every kind of
/// item lives here, plus a flat [`Self::items`] roster used by the
/// sidebar item list. `kind` is the origin tier (project / dep /
/// stdlib) and drives cross-package sort + renderer labelling.
///
/// `pending_extends` holds methods from `extend Type` blocks
/// declared in this package that haven't yet been routed to their
/// target type. [`finalize_project`] drains them once every file
/// has been ingested, so same-package and cross-package targets
/// route identically.
#[derive(Debug)]
pub struct DocPackage {
    pub constants: Vec<DocConstant>,
    pub enums: Vec<DocEnum>,
    pub functions: Vec<DocFunction>,
    pub items: Vec<DocItem>,
    pub kind: PackageKind,
    pub name: String,
    pub protocols: Vec<DocProtocol>,
    pub structs: Vec<DocStruct>,
    pending_extends: Vec<PendingExtend>,
}

/// A method-set from an `extend Type` block, consumed by
/// [`resolve_pending_extends`] before rendering.
#[derive(Debug)]
struct PendingExtend {
    target_package: String,
    target_name: String,
    functions: Vec<DocFunction>,
}

impl DocPackage {
    fn new(name: String, kind: PackageKind) -> Self {
        Self {
            constants: Vec::new(),
            enums: Vec::new(),
            functions: Vec::new(),
            items: Vec::new(),
            kind,
            name,
            protocols: Vec::new(),
            structs: Vec::new(),
            pending_extends: Vec::new(),
        }
    }
}

/// Documentation for an entire project: the user's own package
/// (named in `project_package`) plus any deps and stdlib packages
/// the driver chose to bundle in. The renderer walks
/// [`Self::packages`] to emit one subdir per package.
#[derive(Debug)]
pub struct DocProject {
    /// Bare name of the user's own package — used as the default
    /// landing page and to highlight the project entry in the
    /// sidebar dropdown. Always matches one of `packages[i].name`
    /// once the driver has called `extract_items` for at least one
    /// project source. May be empty when running in loose-file
    /// mode with no `expo.toml`.
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
    /// adopt the supplied `kind`; if the package already exists
    /// the existing kind is preserved (first caller wins). This is
    /// the only way new packages get added to the project — every
    /// `extract_items` call routes through here.
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
/// inside `project`. Items with `@doc false` are excluded; private
/// top-level functions and `extend`-block methods are excluded.
/// `extend Type` blocks queue their methods on the current package's
/// `pending_extends` -- `finalize_project` distributes them to the
/// target package's struct/enum rosters once every file has been
/// ingested. `impl Protocol for Type` blocks do not contribute
/// documentation surface beyond the protocol's own declaration.
pub fn extract_items(file: &File, project: &mut DocProject, package: &str, kind: PackageKind) {
    let pkg = project.ensure_package(package, kind);

    for item in &file.items {
        match item {
            Item::Alias(_) => {}
            Item::Constant(c) => {
                if let Some(dc) = extract_constant(c) {
                    pkg.constants.push(dc);
                }
            }
            Item::Enum(e) => {
                if let Some(de) = extract_enum(e) {
                    pkg.enums.push(de);
                }
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
            Item::Impl(_) => {}
            Item::Protocol(p) => {
                if let Some(dp) = extract_protocol(p) {
                    pkg.protocols.push(dp);
                }
            }
            Item::Struct(s) => {
                if let Some(ds) = extract_struct(s) {
                    pkg.structs.push(ds);
                }
            }
            Item::TypeAlias(_) => {}
        }
    }
}

/// Resolve pending `extend` blocks, sort packages by
/// `(kind tier, name)` so the user's project lands first, then sort
/// and flatten each package's items for the sidebar.
pub fn finalize_project(project: &mut DocProject) {
    resolve_pending_extends(project);

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
        let Some(target) = project
            .packages
            .iter_mut()
            .find(|p| p.name == pending.target_package)
        else {
            continue;
        };
        if let Some(ds) = target
            .structs
            .iter_mut()
            .find(|s| s.name == pending.target_name)
        {
            ds.functions.extend(pending.functions);
        } else if let Some(de) = target
            .enums
            .iter_mut()
            .find(|e| e.name == pending.target_name)
        {
            de.functions.extend(pending.functions);
        }
    }
}

fn finalize_package(pkg: &mut DocPackage) {
    pkg.constants.sort_by(|a, b| a.name.cmp(&b.name));
    pkg.enums.sort_by(|a, b| a.name.cmp(&b.name));
    pkg.functions.sort_by(|a, b| a.name.cmp(&b.name));
    pkg.protocols.sort_by(|a, b| a.name.cmp(&b.name));
    pkg.structs.sort_by(|a, b| a.name.cmp(&b.name));

    for e in &mut pkg.enums {
        e.functions.sort_by(|a, b| a.name.cmp(&b.name));
    }
    for p in &mut pkg.protocols {
        p.functions.sort_by(|a, b| a.name.cmp(&b.name));
    }
    for s in &mut pkg.structs {
        s.functions.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pkg.items.clear();
    for c in &pkg.constants {
        pkg.items.push(DocItem {
            doc: c.doc.clone(),
            kind: "const".to_string(),
            name: c.name.clone(),
        });
    }
    for e in &pkg.enums {
        pkg.items.push(DocItem {
            doc: e.doc.clone(),
            kind: "enum".to_string(),
            name: e.name.clone(),
        });
    }
    for f in &pkg.functions {
        pkg.items.push(DocItem {
            doc: f.doc.clone(),
            kind: "fn".to_string(),
            name: f.name.clone(),
        });
    }
    for p in &pkg.protocols {
        pkg.items.push(DocItem {
            doc: p.doc.clone(),
            kind: "protocol".to_string(),
            name: p.name.clone(),
        });
    }
    for s in &pkg.structs {
        pkg.items.push(DocItem {
            doc: s.doc.clone(),
            kind: "struct".to_string(),
            name: s.name.clone(),
        });
    }
    pkg.items.sort_by(|a, b| a.name.cmp(&b.name));
}

fn annotation_string(annotations: &[expo_ast::ast::Annotation]) -> Option<String> {
    annotations
        .iter()
        .find(|a| a.name == "doc")
        .and_then(|a| match &a.value {
            Some(AnnotationValue::String(s)) => Some(s.clone()),
            _ => None,
        })
}

/// Build a [`PendingExtend`] from an `extend Type` block. Path
/// interpretation mirrors typecheck/IR's `extend_target_path`;
/// inlined so `expo-doc` doesn't need a typecheck dep.
fn make_pending_extend(ext: &ExtendBlock, current_package: &str) -> Option<PendingExtend> {
    let path = match &ext.target {
        TypeExpr::Generic { path, .. } | TypeExpr::Named { path, .. } => path,
        _ => return None,
    };
    let (target_package, target_name) = match path.as_slice() {
        [name] => (current_package.to_string(), name.clone()),
        [head @ .., last] if !head.is_empty() => (head.join("."), last.clone()),
        _ => return None,
    };

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
        target_package,
        target_name,
        functions,
    })
}

fn extract_constant(c: &expo_ast::ast::Constant) -> Option<DocConstant> {
    if has_doc_false(&c.annotations) {
        return None;
    }

    Some(DocConstant {
        doc: annotation_string(&c.annotations),
        name: c.name.clone(),
    })
}

fn extract_enum(e: &EnumDecl) -> Option<DocEnum> {
    if has_doc_false(&e.annotations) {
        return None;
    }

    let variants = e.variants.iter().map(|v| v.name.clone()).collect();
    let functions = e.functions.iter().filter_map(extract_function).collect();

    Some(DocEnum {
        doc: annotation_string(&e.annotations),
        functions,
        name: e.name.clone(),
        variants,
    })
}

fn extract_function(f: &Function) -> Option<DocFunction> {
    if f.visibility == Visibility::Private || has_doc_false(&f.annotations) {
        return None;
    }

    let params = extract_params(&f.params);

    Some(DocFunction {
        doc: annotation_string(&f.annotations),
        name: f.name.clone(),
        params,
        return_type: f.return_type.as_ref().map(type_expr_to_string),
        type_params: f.type_params.iter().map(|tp| tp.name.clone()).collect(),
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
                name: name.clone(),
                type_name: type_expr_to_string(type_expr),
            },
        })
        .collect()
}

fn extract_protocol(p: &ProtocolDecl) -> Option<DocProtocol> {
    if has_doc_false(&p.annotations) {
        return None;
    }

    let functions = p
        .methods
        .iter()
        .filter_map(extract_protocol_method)
        .collect();

    Some(DocProtocol {
        doc: annotation_string(&p.annotations),
        functions,
        name: p.name.clone(),
        type_params: p.type_params.iter().map(|tp| tp.name.clone()).collect(),
    })
}

fn extract_protocol_method(m: &ProtocolMethod) -> Option<DocFunction> {
    if has_doc_false(&m.annotations) {
        return None;
    }

    let params = extract_params(&m.params);

    Some(DocFunction {
        doc: annotation_string(&m.annotations),
        name: m.name.clone(),
        params,
        return_type: m.return_type.as_ref().map(type_expr_to_string),
        type_params: m.type_params.iter().map(|tp| tp.name.clone()).collect(),
    })
}

fn extract_struct(s: &StructDecl) -> Option<DocStruct> {
    if has_doc_false(&s.annotations) {
        return None;
    }

    let fields = s
        .fields
        .iter()
        .map(|f| DocField {
            name: f.name.clone(),
            type_name: type_expr_to_string(&f.type_expr),
        })
        .collect();
    let functions = s.functions.iter().filter_map(extract_function).collect();

    Some(DocStruct {
        doc: annotation_string(&s.annotations),
        fields,
        functions,
        name: s.name.clone(),
        type_params: s.type_params.iter().map(|tp| tp.name.clone()).collect(),
    })
}

fn has_doc_false(annotations: &[expo_ast::ast::Annotation]) -> bool {
    annotations
        .iter()
        .any(|a| a.name == "doc" && a.value == Some(AnnotationValue::False))
}

/// Format a type expression as a human-readable string.
fn type_expr_to_string(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named { path, .. } => path.join("."),
        TypeExpr::Generic { path, args, .. } => {
            let args_str: Vec<String> = args.iter().map(type_expr_to_string).collect();
            format!("{}<{}>", path.join("."), args_str.join(", "))
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
        TypeExpr::Union { types, .. } => {
            let parts: Vec<String> = types.iter().map(type_expr_to_string).collect();
            parts.join(" | ")
        }
    }
}
