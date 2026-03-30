//! Render documentation structs into static HTML pages using askama templates.

use askama::Template;

use crate::extract::{
    DocConstant, DocEnum, DocFunction, DocItem, DocModule, DocProtocol, DocStruct,
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
#[template(path = "module.html")]
struct ModuleTemplate<'a> {
    active_item: Option<&'a str>,
    all_modules: &'a [DocModule],
    css: &'a str,
    is_item_page: bool,
    items: &'a [DocItem],
    module_name: &'a str,
    moduledoc: Option<&'a str>,
}

#[derive(Template)]
#[template(path = "item_struct.html")]
struct StructTemplate<'a> {
    active_item: Option<&'a str>,
    all_modules: &'a [DocModule],
    css: &'a str,
    is_item_page: bool,
    module_name: &'a str,
    s: &'a DocStruct,
}

#[derive(Template)]
#[template(path = "item_enum.html")]
struct EnumTemplate<'a> {
    active_item: Option<&'a str>,
    all_modules: &'a [DocModule],
    css: &'a str,
    e: &'a DocEnum,
    is_item_page: bool,
    module_name: &'a str,
}

#[derive(Template)]
#[template(path = "item_protocol.html")]
struct ProtocolTemplate<'a> {
    active_item: Option<&'a str>,
    all_modules: &'a [DocModule],
    css: &'a str,
    is_item_page: bool,
    module_name: &'a str,
    p: &'a DocProtocol,
}

#[derive(Template)]
#[template(path = "item_function.html")]
struct FunctionTemplate<'a> {
    active_item: Option<&'a str>,
    all_modules: &'a [DocModule],
    css: &'a str,
    f: &'a DocFunction,
    is_item_page: bool,
    module_name: &'a str,
}

#[derive(Template)]
#[template(path = "item_constant.html")]
struct ConstantTemplate<'a> {
    active_item: Option<&'a str>,
    all_modules: &'a [DocModule],
    c: &'a DocConstant,
    css: &'a str,
    is_item_page: bool,
    module_name: &'a str,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate<'a> {
    redirect_to: &'a str,
}

/// Render a module index page as a complete HTML string.
pub fn render_module(module: &DocModule, all_modules: &[DocModule]) -> String {
    let tmpl = ModuleTemplate {
        active_item: None,
        all_modules,
        css: CSS,
        is_item_page: false,
        items: &module.items,
        module_name: &module.name,
        moduledoc: module.moduledoc.as_deref(),
    };
    tmpl.render().expect("failed to render module template")
}

pub fn render_struct(module: &DocModule, s: &DocStruct, all_modules: &[DocModule]) -> String {
    let tmpl = StructTemplate {
        css: CSS,
        module_name: &module.name,
        s,
        all_modules,
        active_item: Some(&s.name),
        is_item_page: true,
    };
    tmpl.render().expect("failed to render struct template")
}

pub fn render_constant(module: &DocModule, c: &DocConstant, all_modules: &[DocModule]) -> String {
    let tmpl = ConstantTemplate {
        css: CSS,
        module_name: &module.name,
        c,
        all_modules,
        active_item: Some(&c.name),
        is_item_page: true,
    };
    tmpl.render().expect("failed to render constant template")
}

pub fn render_enum(module: &DocModule, e: &DocEnum, all_modules: &[DocModule]) -> String {
    let tmpl = EnumTemplate {
        css: CSS,
        module_name: &module.name,
        e,
        all_modules,
        active_item: Some(&e.name),
        is_item_page: true,
    };
    tmpl.render().expect("failed to render enum template")
}

pub fn render_function(module: &DocModule, f: &DocFunction, all_modules: &[DocModule]) -> String {
    let tmpl = FunctionTemplate {
        css: CSS,
        module_name: &module.name,
        f,
        all_modules,
        active_item: Some(&f.name),
        is_item_page: true,
    };
    tmpl.render().expect("failed to render function template")
}

pub fn render_protocol(module: &DocModule, p: &DocProtocol, all_modules: &[DocModule]) -> String {
    let tmpl = ProtocolTemplate {
        css: CSS,
        module_name: &module.name,
        p,
        all_modules,
        active_item: Some(&p.name),
        is_item_page: true,
    };
    tmpl.render().expect("failed to render protocol template")
}

/// Render an index page that redirects to the first module.
pub fn render_index(modules: &[DocModule]) -> String {
    let target = modules
        .first()
        .map(|m| format!("{}.html", m.name))
        .unwrap_or_default();
    let tmpl = IndexTemplate {
        redirect_to: &target,
    };
    tmpl.render().expect("failed to render index template")
}
