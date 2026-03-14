//! Render documentation structs into static HTML pages using askama templates.

use askama::Template;

use crate::extract::{DocConstant, DocEnum, DocFunction, DocModule, DocStruct};
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
    css: &'a str,
    name: &'a str,
    moduledoc: Option<&'a str>,
    structs: &'a [DocStruct],
    enums: &'a [DocEnum],
    functions: &'a [DocFunction],
    constants: &'a [DocConstant],
    all_modules: &'a [String],
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexTemplate<'a> {
    redirect_to: &'a str,
}

/// Render a module documentation page as a complete HTML string.
///
/// `all_module_names` provides the full list of module names for the sidebar
/// navigation, allowing cross-module navigation from any page.
pub fn render_module(module: &DocModule, all_module_names: &[String]) -> String {
    let tmpl = ModuleTemplate {
        css: CSS,
        name: &module.name,
        moduledoc: module.moduledoc.as_deref(),
        structs: &module.structs,
        enums: &module.enums,
        functions: &module.functions,
        constants: &module.constants,
        all_modules: all_module_names,
    };
    tmpl.render().expect("failed to render module template")
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
