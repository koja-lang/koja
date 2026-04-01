//! Render documentation structs into static HTML pages using askama templates.

use askama::Template;

use crate::extract::{
    DocConstant, DocEnum, DocFunction, DocItem, DocProject, DocProtocol, DocStruct,
};
use crate::style::CSS;

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

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate<'a> {
    active_item: Option<&'a str>,
    all_items: &'a [DocItem],
    css: &'a str,
    items: &'a [DocItem],
}

#[derive(Template)]
#[template(path = "item_struct.html")]
struct StructTemplate<'a> {
    active_item: Option<&'a str>,
    all_items: &'a [DocItem],
    css: &'a str,
    s: &'a DocStruct,
}

#[derive(Template)]
#[template(path = "item_enum.html")]
struct EnumTemplate<'a> {
    active_item: Option<&'a str>,
    all_items: &'a [DocItem],
    css: &'a str,
    e: &'a DocEnum,
}

#[derive(Template)]
#[template(path = "item_protocol.html")]
struct ProtocolTemplate<'a> {
    active_item: Option<&'a str>,
    all_items: &'a [DocItem],
    css: &'a str,
    p: &'a DocProtocol,
}

#[derive(Template)]
#[template(path = "item_function.html")]
struct FunctionTemplate<'a> {
    active_item: Option<&'a str>,
    all_items: &'a [DocItem],
    css: &'a str,
    f: &'a DocFunction,
}

#[derive(Template)]
#[template(path = "item_constant.html")]
struct ConstantTemplate<'a> {
    active_item: Option<&'a str>,
    all_items: &'a [DocItem],
    c: &'a DocConstant,
    css: &'a str,
}

/// Render the main index page listing all items.
pub fn render_index(project: &DocProject) -> String {
    let tmpl = IndexTemplate {
        active_item: None,
        all_items: &project.items,
        css: CSS,
        items: &project.items,
    };
    tmpl.render().expect("failed to render index template")
}

pub fn render_struct(s: &DocStruct, project: &DocProject) -> String {
    let tmpl = StructTemplate {
        css: CSS,
        s,
        all_items: &project.items,
        active_item: Some(&s.name),
    };
    tmpl.render().expect("failed to render struct template")
}

pub fn render_constant(c: &DocConstant, project: &DocProject) -> String {
    let tmpl = ConstantTemplate {
        css: CSS,
        c,
        all_items: &project.items,
        active_item: Some(&c.name),
    };
    tmpl.render().expect("failed to render constant template")
}

pub fn render_enum(e: &DocEnum, project: &DocProject) -> String {
    let tmpl = EnumTemplate {
        css: CSS,
        e,
        all_items: &project.items,
        active_item: Some(&e.name),
    };
    tmpl.render().expect("failed to render enum template")
}

pub fn render_function(f: &DocFunction, project: &DocProject) -> String {
    let tmpl = FunctionTemplate {
        css: CSS,
        f,
        all_items: &project.items,
        active_item: Some(&f.name),
    };
    tmpl.render().expect("failed to render function template")
}

pub fn render_protocol(p: &DocProtocol, project: &DocProject) -> String {
    let tmpl = ProtocolTemplate {
        css: CSS,
        p,
        all_items: &project.items,
        active_item: Some(&p.name),
    };
    tmpl.render().expect("failed to render protocol template")
}
