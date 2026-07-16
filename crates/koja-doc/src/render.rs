//! Render documentation structs into static HTML pages using
//! Askama templates. The renderer is package-aware. Every page
//! receives the sidebar context (package roster + current
//! package's item list + active item) so a single sidebar
//! template can serve the root, per-package, and per-item pages.

use askama::Template;

use crate::extract::{
    DocConstant, DocEnum, DocFunction, DocItem, DocPackage, DocProject, DocProtocol, DocStruct,
    PackageKind,
};

mod filters {
    use std::fmt::Display;

    use askama::Values;
    use askama::filters::Safe;
    use pulldown_cmark::{Options, Parser, html};

    /// Render a markdown string to HTML. Returns `Safe` to skip auto-escaping.
    #[askama::filter_fn]
    pub fn md(s: impl Display, _env: &dyn Values) -> askama::Result<Safe<String>> {
        let input = s.to_string();
        let parser = Parser::new_ext(input.trim(), Options::all());
        let mut output = String::new();
        html::push_html(&mut output, parser);
        Ok(Safe(output))
    }

    /// Extract the first sentence from a doc string for summary display.
    #[askama::filter_fn]
    pub fn brief(s: impl Display, _env: &dyn Values) -> askama::Result<String> {
        let trimmed = s.to_string();
        let trimmed = trimmed.trim();
        let result = if let Some(idx) = trimmed.find(". ") {
            trimmed[..=idx].to_string()
        } else if let Some(idx) = trimmed.find(".\n") {
            trimmed[..=idx].to_string()
        } else if trimmed.ends_with('.') {
            trimmed.to_string()
        } else {
            trimmed.lines().next().unwrap_or("").to_string()
        };
        Ok(result)
    }
}

/// Sidebar dropdown entry. One per [`DocPackage`] in the project,
/// in the same sort order [`crate::extract::finalize_project`]
/// stamped onto `project.packages`.
#[derive(Debug)]
pub struct PackageRef<'a> {
    /// Bare package name (matches the subdir name on disk).
    pub name: &'a str,
    /// Origin tier label used as the package chip text on the
    /// root-roster page (`"project"` / `"dependency"` / `"stdlib"`).
    pub kind_label: &'static str,
    /// Count of documentable items in the package. Drives the
    /// brief on the root-roster row ("123 items").
    pub item_count: usize,
    /// `"s"` when `item_count != 1` so the template can produce
    /// "1 item" / "2 items" without inline conditionals.
    pub item_plural: &'static str,
}

impl<'a> PackageRef<'a> {
    fn from_package(pkg: &'a DocPackage) -> Self {
        let item_count = pkg.items.len();
        Self {
            name: &pkg.name,
            kind_label: pkg.kind.label(),
            item_count,
            item_plural: if item_count == 1 { "" } else { "s" },
        }
    }
}

#[derive(Template)]
#[template(path = "index.html")]
struct RootIndexTemplate<'a> {
    active_item: Option<&'a str>,
    current_package: Option<&'a str>,
    dep_count: usize,
    dep_plural: &'static str,
    packages: Vec<PackageRef<'a>>,
    project_name: &'a str,
    root_prefix: &'a str,
    sidebar_items: &'a [DocItem],
    stdlib_count: usize,
    stdlib_plural: &'static str,
}

#[derive(Template)]
#[template(path = "package_index.html")]
struct PackageIndexTemplate<'a> {
    active_item: Option<&'a str>,
    current_package: Option<&'a str>,
    package_kind_label: &'static str,
    package_name: &'a str,
    packages: Vec<PackageRef<'a>>,
    project_name: &'a str,
    root_prefix: &'a str,
    sidebar_items: &'a [DocItem],
}

#[derive(Template)]
#[template(path = "item_struct.html")]
struct StructTemplate<'a> {
    active_item: Option<&'a str>,
    current_package: Option<&'a str>,
    packages: Vec<PackageRef<'a>>,
    project_name: &'a str,
    root_prefix: &'a str,
    s: &'a DocStruct,
    sidebar_items: &'a [DocItem],
}

#[derive(Template)]
#[template(path = "item_enum.html")]
struct EnumTemplate<'a> {
    active_item: Option<&'a str>,
    current_package: Option<&'a str>,
    e: &'a DocEnum,
    packages: Vec<PackageRef<'a>>,
    project_name: &'a str,
    root_prefix: &'a str,
    sidebar_items: &'a [DocItem],
}

#[derive(Template)]
#[template(path = "item_protocol.html")]
struct ProtocolTemplate<'a> {
    active_item: Option<&'a str>,
    current_package: Option<&'a str>,
    p: &'a DocProtocol,
    packages: Vec<PackageRef<'a>>,
    project_name: &'a str,
    root_prefix: &'a str,
    sidebar_items: &'a [DocItem],
}

#[derive(Template)]
#[template(path = "item_function.html")]
struct FunctionTemplate<'a> {
    active_item: Option<&'a str>,
    current_package: Option<&'a str>,
    f: &'a DocFunction,
    packages: Vec<PackageRef<'a>>,
    project_name: &'a str,
    root_prefix: &'a str,
    sidebar_items: &'a [DocItem],
}

#[derive(Template)]
#[template(path = "item_constant.html")]
struct ConstantTemplate<'a> {
    active_item: Option<&'a str>,
    c: &'a DocConstant,
    current_package: Option<&'a str>,
    packages: Vec<PackageRef<'a>>,
    project_name: &'a str,
    root_prefix: &'a str,
    sidebar_items: &'a [DocItem],
}

/// Build the package-roster sidebar context, the same input every
/// page receives at the root level. `sidebar_items` is empty
/// because the root page lists packages, not items.
fn package_refs(project: &DocProject) -> Vec<PackageRef<'_>> {
    project
        .packages
        .iter()
        .map(PackageRef::from_package)
        .collect()
}

fn dep_stats(project: &DocProject) -> (usize, &'static str, usize, &'static str) {
    let dep_count = project
        .packages
        .iter()
        .filter(|p| p.kind == PackageKind::Dependency)
        .count();
    let stdlib_count = project
        .packages
        .iter()
        .filter(|p| p.kind == PackageKind::Stdlib)
        .count();
    let dep_plural = if dep_count == 1 { "" } else { "s" };
    let stdlib_plural = if stdlib_count == 1 { "" } else { "s" };
    (dep_count, dep_plural, stdlib_count, stdlib_plural)
}

const EMPTY_ITEMS: &[DocItem] = &[];

/// Render the top-level `doc/index.html`, the package roster.
pub fn render_root_index(project: &DocProject) -> String {
    let (dep_count, dep_plural, stdlib_count, stdlib_plural) = dep_stats(project);
    let tmpl = RootIndexTemplate {
        active_item: None,
        current_package: None,
        dep_count,
        dep_plural,
        packages: package_refs(project),
        project_name: &project.project_package,
        root_prefix: "",
        sidebar_items: EMPTY_ITEMS,
        stdlib_count,
        stdlib_plural,
    };
    tmpl.render().expect("failed to render root index template")
}

/// Render `doc/<Pkg>/index.html`, a single package's overview.
pub fn render_package_index(pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = PackageIndexTemplate {
        active_item: None,
        current_package: Some(&pkg.name),
        package_kind_label: pkg.kind.label(),
        package_name: &pkg.name,
        packages: package_refs(project),
        project_name: &project.project_package,
        root_prefix: "../",
        sidebar_items: &pkg.items,
    };
    tmpl.render()
        .expect("failed to render package index template")
}

pub fn render_struct(s: &DocStruct, pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = StructTemplate {
        active_item: Some(&s.name),
        current_package: Some(&pkg.name),
        packages: package_refs(project),
        project_name: &project.project_package,
        root_prefix: "../",
        s,
        sidebar_items: &pkg.items,
    };
    tmpl.render().expect("failed to render struct template")
}

pub fn render_constant(c: &DocConstant, pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = ConstantTemplate {
        active_item: Some(&c.name),
        c,
        current_package: Some(&pkg.name),
        packages: package_refs(project),
        project_name: &project.project_package,
        root_prefix: "../",
        sidebar_items: &pkg.items,
    };
    tmpl.render().expect("failed to render constant template")
}

pub fn render_enum(e: &DocEnum, pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = EnumTemplate {
        active_item: Some(&e.name),
        current_package: Some(&pkg.name),
        e,
        packages: package_refs(project),
        project_name: &project.project_package,
        root_prefix: "../",
        sidebar_items: &pkg.items,
    };
    tmpl.render().expect("failed to render enum template")
}

pub fn render_function(f: &DocFunction, pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = FunctionTemplate {
        active_item: Some(&f.name),
        current_package: Some(&pkg.name),
        f,
        packages: package_refs(project),
        project_name: &project.project_package,
        root_prefix: "../",
        sidebar_items: &pkg.items,
    };
    tmpl.render().expect("failed to render function template")
}

pub fn render_protocol(p: &DocProtocol, pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = ProtocolTemplate {
        active_item: Some(&p.name),
        current_package: Some(&pkg.name),
        p,
        packages: package_refs(project),
        project_name: &project.project_package,
        root_prefix: "../",
        sidebar_items: &pkg.items,
    };
    tmpl.render().expect("failed to render protocol template")
}
