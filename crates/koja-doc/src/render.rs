//! Render documentation structs into static HTML pages using
//! Askama templates. Every page receives a [`PageContext`] with
//! the top-bar package roster, the left rail's grouped item list,
//! and the right "on this page" TOC. The three-column frame is
//! identical on every page. A page with no TOC entries leaves the
//! third column blank rather than reflowing the article.

use askama::Template;

use crate::extract::{
    DocBuiltin, DocConstant, DocEnum, DocFunction, DocItem, DocPackage, DocProject, DocProtocol,
    DocStruct, PackageKind,
};

mod filters {
    use std::fmt::Display;

    use askama::Values;
    use askama::filters::Safe;
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd, html};

    use crate::highlight::highlight_koja;

    /// Render a markdown string to HTML, running fenced code
    /// blocks through the Koja highlighter. Returns `Safe` to
    /// skip auto-escaping.
    #[askama::filter_fn]
    pub fn md(s: impl Display, _env: &dyn Values) -> askama::Result<Safe<String>> {
        let input = s.to_string();
        let parser = Parser::new_ext(input.trim(), Options::all());

        let mut events: Vec<Event> = Vec::new();
        let mut code_block: Option<(bool, String)> = None;
        for event in parser {
            match event {
                Event::Start(Tag::CodeBlock(kind)) => {
                    let is_koja = match &kind {
                        CodeBlockKind::Fenced(lang) => {
                            lang.is_empty() || matches!(lang.as_ref(), "koja" | "kojs")
                        }
                        CodeBlockKind::Indented => true,
                    };
                    code_block = Some((is_koja, String::new()));
                }
                Event::Text(text) if code_block.is_some() => {
                    code_block
                        .as_mut()
                        .expect("checked in guard")
                        .1
                        .push_str(&text);
                }
                Event::End(TagEnd::CodeBlock) => {
                    let (is_koja, code) = code_block.take().expect("start precedes end");
                    let body = if is_koja {
                        highlight_koja(&code)
                    } else {
                        escape_html(&code)
                    };
                    events.push(Event::Html(
                        format!("<pre class=\"codeblock\"><code>{body}</code></pre>").into(),
                    ));
                }
                other => events.push(other),
            }
        }

        let mut output = String::new();
        html::push_html(&mut output, events.into_iter());
        Ok(Safe(output))
    }

    fn escape_html(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;")
    }

    /// Extract the first sentence from a doc string and render its
    /// inline markdown (code spans, emphasis) for summary display.
    #[askama::filter_fn]
    pub fn brief(s: impl Display, _env: &dyn Values) -> askama::Result<Safe<String>> {
        let text = s.to_string();
        let trimmed = text.trim();
        let sentence = if let Some(idx) = trimmed.find(". ") {
            &trimmed[..=idx]
        } else if let Some(idx) = trimmed.find(".\n") {
            &trimmed[..=idx]
        } else if trimmed.ends_with('.') {
            trimmed
        } else {
            trimmed.lines().next().unwrap_or("")
        };

        let parser = Parser::new_ext(sentence, Options::all());
        let mut rendered = String::new();
        html::push_html(&mut rendered, parser);
        let rendered = rendered.trim();
        let inline = rendered
            .strip_prefix("<p>")
            .and_then(|r| r.strip_suffix("</p>"))
            .unwrap_or(rendered);
        Ok(Safe(inline.to_string()))
    }
}

/// Top-bar dropdown entry. One per [`DocPackage`] in the project,
/// in the same sort order [`crate::extract::finalize_project`]
/// stamped onto `project.packages`.
#[derive(Debug)]
pub struct PackageRef<'a> {
    /// Bare package name (matches the subdir name on disk).
    pub name: &'a str,
    /// Origin tier label (`"project"` / `"dependency"` / `"stdlib"`),
    /// shown as muted text on the root roster.
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

/// One left-rail section. `label` is `None` for the flat list
/// small packages get.
struct SidebarGroup<'a> {
    label: Option<&'static str>,
    items: Vec<&'a DocItem>,
}

/// Item count above which the left rail groups items by kind
/// instead of showing one flat alphabetical list.
const GROUP_THRESHOLD: usize = 8;

fn sidebar_groups(items: &[DocItem]) -> Vec<SidebarGroup<'_>> {
    if items.is_empty() {
        return Vec::new();
    }
    if items.len() <= GROUP_THRESHOLD {
        return vec![SidebarGroup {
            label: None,
            items: items.iter().collect(),
        }];
    }

    let kinds: [(&str, &'static str); 6] = [
        ("builtin", "Builtins"),
        ("const", "Constants"),
        ("enum", "Enums"),
        ("fn", "Functions"),
        ("protocol", "Protocols"),
        ("struct", "Structs"),
    ];
    kinds
        .iter()
        .filter_map(|(kind, label)| {
            let group: Vec<&DocItem> = items.iter().filter(|i| i.kind == *kind).collect();
            (!group.is_empty()).then_some(SidebarGroup {
                label: Some(label),
                items: group,
            })
        })
        .collect()
}

/// One link in the right "on this page" TOC. Section links
/// ("Fields", "Variants") render in the body font and function
/// links render in mono.
struct TocEntry {
    href: String,
    label: String,
    mono: bool,
}

impl TocEntry {
    fn section(label: &str, href: &str) -> Self {
        Self {
            href: href.to_string(),
            label: label.to_string(),
            mono: false,
        }
    }

    fn function(name: &str) -> Self {
        Self {
            href: format!("fn-{name}"),
            label: name.to_string(),
            mono: true,
        }
    }
}

fn function_entries(functions: &[DocFunction]) -> impl Iterator<Item = TocEntry> + '_ {
    functions.iter().map(|f| TocEntry::function(&f.name))
}

fn builtin_toc(b: &DocBuiltin) -> Vec<TocEntry> {
    function_entries(&b.functions).collect()
}

fn struct_toc(s: &DocStruct) -> Vec<TocEntry> {
    let mut toc = Vec::new();
    if !s.fields.is_empty() {
        toc.push(TocEntry::section("Fields", "fields"));
    }
    toc.extend(function_entries(&s.functions));
    toc
}

fn enum_toc(e: &DocEnum) -> Vec<TocEntry> {
    let mut toc = Vec::new();
    if !e.variants.is_empty() {
        toc.push(TocEntry::section("Variants", "variants"));
    }
    toc.extend(function_entries(&e.functions));
    toc
}

fn protocol_toc(p: &DocProtocol) -> Vec<TocEntry> {
    function_entries(&p.functions).collect()
}

/// Shared context for the top bar, left rail, and right TOC that
/// every page template receives.
struct PageContext<'a> {
    active_item: Option<&'a str>,
    current_package: Option<&'a str>,
    packages: Vec<PackageRef<'a>>,
    project_name: &'a str,
    root_prefix: &'a str,
    sidebar_groups: Vec<SidebarGroup<'a>>,
    toc: Vec<TocEntry>,
}

impl<'a> PageContext<'a> {
    fn root(project: &'a DocProject) -> Self {
        Self {
            active_item: None,
            current_package: None,
            packages: package_refs(project),
            project_name: &project.project_package,
            root_prefix: "",
            sidebar_groups: Vec::new(),
            toc: Vec::new(),
        }
    }

    fn package(pkg: &'a DocPackage, project: &'a DocProject) -> Self {
        Self {
            active_item: None,
            current_package: Some(&pkg.name),
            packages: package_refs(project),
            project_name: &project.project_package,
            root_prefix: "../",
            sidebar_groups: sidebar_groups(&pkg.items),
            toc: Vec::new(),
        }
    }

    fn item(
        active: &'a str,
        toc: Vec<TocEntry>,
        pkg: &'a DocPackage,
        project: &'a DocProject,
    ) -> Self {
        Self {
            active_item: Some(active),
            toc,
            ..Self::package(pkg, project)
        }
    }
}

#[derive(Template)]
#[template(path = "index.html")]
struct RootIndexTemplate<'a> {
    ctx: PageContext<'a>,
    dep_count: usize,
    dep_plural: &'static str,
    stdlib_count: usize,
    stdlib_plural: &'static str,
}

#[derive(Template)]
#[template(path = "package_index.html")]
struct PackageIndexTemplate<'a> {
    ctx: PageContext<'a>,
    items: &'a [DocItem],
    package_kind_label: &'static str,
    package_name: &'a str,
}

#[derive(Template)]
#[template(path = "item_builtin.html")]
struct BuiltinTemplate<'a> {
    b: &'a DocBuiltin,
    ctx: PageContext<'a>,
}

#[derive(Template)]
#[template(path = "item_struct.html")]
struct StructTemplate<'a> {
    ctx: PageContext<'a>,
    s: &'a DocStruct,
}

#[derive(Template)]
#[template(path = "item_enum.html")]
struct EnumTemplate<'a> {
    ctx: PageContext<'a>,
    e: &'a DocEnum,
}

#[derive(Template)]
#[template(path = "item_protocol.html")]
struct ProtocolTemplate<'a> {
    ctx: PageContext<'a>,
    p: &'a DocProtocol,
}

#[derive(Template)]
#[template(path = "item_function.html")]
struct FunctionTemplate<'a> {
    ctx: PageContext<'a>,
    f: &'a DocFunction,
}

#[derive(Template)]
#[template(path = "item_constant.html")]
struct ConstantTemplate<'a> {
    ctx: PageContext<'a>,
    c: &'a DocConstant,
}

/// Build the package-roster context, the same input every page
/// receives at the root level.
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

/// Render the top-level `doc/index.html`, the package roster.
pub fn render_root_index(project: &DocProject) -> String {
    let (dep_count, dep_plural, stdlib_count, stdlib_plural) = dep_stats(project);
    let tmpl = RootIndexTemplate {
        ctx: PageContext::root(project),
        dep_count,
        dep_plural,
        stdlib_count,
        stdlib_plural,
    };
    tmpl.render().expect("failed to render root index template")
}

/// Render `doc/<Pkg>/index.html`, a single package's overview.
pub fn render_package_index(pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = PackageIndexTemplate {
        ctx: PageContext::package(pkg, project),
        items: &pkg.items,
        package_kind_label: pkg.kind.label(),
        package_name: &pkg.name,
    };
    tmpl.render()
        .expect("failed to render package index template")
}

pub fn render_builtin(b: &DocBuiltin, pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = BuiltinTemplate {
        b,
        ctx: PageContext::item(&b.name, builtin_toc(b), pkg, project),
    };
    tmpl.render().expect("failed to render builtin template")
}

pub fn render_struct(s: &DocStruct, pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = StructTemplate {
        ctx: PageContext::item(&s.name, struct_toc(s), pkg, project),
        s,
    };
    tmpl.render().expect("failed to render struct template")
}

pub fn render_constant(c: &DocConstant, pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = ConstantTemplate {
        ctx: PageContext::item(&c.name, Vec::new(), pkg, project),
        c,
    };
    tmpl.render().expect("failed to render constant template")
}

pub fn render_enum(e: &DocEnum, pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = EnumTemplate {
        ctx: PageContext::item(&e.name, enum_toc(e), pkg, project),
        e,
    };
    tmpl.render().expect("failed to render enum template")
}

pub fn render_function(f: &DocFunction, pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = FunctionTemplate {
        ctx: PageContext::item(&f.name, Vec::new(), pkg, project),
        f,
    };
    tmpl.render().expect("failed to render function template")
}

pub fn render_protocol(p: &DocProtocol, pkg: &DocPackage, project: &DocProject) -> String {
    let tmpl = ProtocolTemplate {
        ctx: PageContext::item(&p.name, protocol_toc(p), pkg, project),
        p,
    };
    tmpl.render().expect("failed to render protocol template")
}
