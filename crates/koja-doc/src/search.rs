//! Build the `search-index.json` payload powering the sidebar
//! fuzzy search. One entry per item (builtin / struct / enum /
//! protocol / top-level fn / constant) and one entry per method on a type
//! (deep-linking to `#fn-<method>` on the type's page). The JSON
//! is also the AI-friendly bundle, where every doc-visible symbol in
//! the project + bundled stdlib + bundled deps is present with
//! its kind, owning package, URL, and brief.
//!
//! We hand-roll the JSON encoding to avoid an extra workspace
//! dependency on `serde_json`. The payload shape is fixed and
//! the only escaping concern is doc-string content.

use crate::extract::{
    DocBuiltin, DocConstant, DocEnum, DocFunction, DocPackage, DocProject, DocProtocol, DocStruct,
};

/// Format `project` as the contents of `doc/search-index.json`,
/// ready to be written verbatim by the driver. Same sort order
/// the renderer uses for the per-package item lists, so an
/// alphabetical hit list matches the visible sidebar when the
/// search box is empty.
pub fn search_index_json(project: &DocProject) -> String {
    let symbols = collect_symbols(project);

    let mut out = String::from("[");
    for (idx, symbol) in symbols.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str("\n  {");
        out.push_str(&format!("\"pkg\":{},", json_str(symbol.package)));
        out.push_str(&format!("\"name\":{},", json_str(&symbol.name)));
        out.push_str(&format!("\"kind\":{},", json_str(symbol.kind)));
        out.push_str(&format!("\"url\":{},", json_str(&symbol.url())));
        out.push_str(&format!("\"brief\":{},", json_str(&symbol.brief())));
        match symbol.deprecated() {
            Some(message) => {
                out.push_str(&format!("\"deprecated\":{}", json_str(message)));
            }
            None => out.push_str("\"deprecated\":null"),
        }
        out.push('}');
    }
    if !symbols.is_empty() {
        out.push('\n');
    }
    out.push(']');
    out.push('\n');
    out
}

/// One doc-visible symbol, shared between the JSON search index
/// and the terminal matcher. `name` is the display name, either a
/// bare item name (`List`) or a member spelling (`List.append`).
pub(crate) struct Symbol<'a> {
    pub kind: &'static str,
    pub name: String,
    pub owner: Option<&'a str>,
    pub package: &'a str,
    pub target: SymbolTarget<'a>,
}

/// The doc item a [`Symbol`] points at, for full-doc rendering.
pub(crate) enum SymbolTarget<'a> {
    Builtin(&'a DocBuiltin),
    Constant(&'a DocConstant),
    Enum(&'a DocEnum),
    Function(&'a DocFunction),
    Protocol(&'a DocProtocol),
    Struct(&'a DocStruct),
}

impl Symbol<'_> {
    pub fn brief(&self) -> String {
        brief(self.doc())
    }

    pub fn doc(&self) -> &Option<String> {
        match &self.target {
            SymbolTarget::Builtin(b) => &b.doc,
            SymbolTarget::Constant(c) => &c.doc,
            SymbolTarget::Enum(e) => &e.doc,
            SymbolTarget::Function(f) => &f.doc,
            SymbolTarget::Protocol(p) => &p.doc,
            SymbolTarget::Struct(s) => &s.doc,
        }
    }

    pub fn deprecated(&self) -> Option<&str> {
        match &self.target {
            SymbolTarget::Builtin(b) => b.deprecated.as_deref(),
            SymbolTarget::Constant(c) => c.deprecated.as_deref(),
            SymbolTarget::Enum(e) => e.deprecated.as_deref(),
            SymbolTarget::Function(f) => f.deprecated.as_deref(),
            SymbolTarget::Protocol(p) => p.deprecated.as_deref(),
            SymbolTarget::Struct(s) => s.deprecated.as_deref(),
        }
    }

    pub fn qualified_name(&self) -> String {
        format!("{}.{}", self.package, self.name)
    }

    fn url(&self) -> String {
        match (&self.target, self.owner) {
            (SymbolTarget::Function(f), Some(owner)) => {
                format!("{}/{owner}.html#{}", self.package, f.anchor())
            }
            (SymbolTarget::Function(f), None) => {
                format!("{}/{}.html", self.package, f.page_name())
            }
            _ => format!("{}/{}.html", self.package, self.name),
        }
    }
}

/// Enumerate every doc-visible symbol in roster order, walking each
/// package's builtins, constants, enums, functions, protocols, then
/// structs.
pub(crate) fn collect_symbols(project: &DocProject) -> Vec<Symbol<'_>> {
    let mut out = Vec::new();
    for pkg in &project.packages {
        collect_package_symbols(pkg, &mut out);
    }
    out
}

fn collect_package_symbols<'a>(pkg: &'a DocPackage, out: &mut Vec<Symbol<'a>>) {
    let item = |kind, name: &str, target| Symbol {
        kind,
        name: name.to_string(),
        owner: None,
        package: &pkg.name,
        target,
    };
    let member = |owner: &'a str, f: &'a DocFunction| Symbol {
        kind: "fn",
        name: format!("{owner}.{}", f.display_name()),
        owner: Some(owner),
        package: &pkg.name,
        target: SymbolTarget::Function(f),
    };

    for b in &pkg.builtins {
        out.push(item("builtin", &b.name, SymbolTarget::Builtin(b)));
        out.extend(b.functions.iter().map(|f| member(&b.name, f)));
    }
    for c in &pkg.constants {
        out.push(item("const", &c.name, SymbolTarget::Constant(c)));
    }
    for e in &pkg.enums {
        out.push(item("enum", &e.name, SymbolTarget::Enum(e)));
        out.extend(e.functions.iter().map(|f| member(&e.name, f)));
    }
    for f in &pkg.functions {
        out.push(item("fn", &f.display_name(), SymbolTarget::Function(f)));
    }
    for p in &pkg.protocols {
        out.push(item("protocol", &p.name, SymbolTarget::Protocol(p)));
        out.extend(p.functions.iter().map(|f| member(&p.name, f)));
    }
    for s in &pkg.structs {
        out.push(item("struct", &s.name, SymbolTarget::Struct(s)));
        out.extend(s.functions.iter().map(|f| member(&s.name, f)));
    }
}

/// Mirror of [`crate::render::filters::brief`]. The search payload
/// doesn't render through a template, so we re-derive the
/// first-sentence brief here.
fn brief(doc: &Option<String>) -> String {
    let Some(doc) = doc else {
        return String::new();
    };
    let trimmed = doc.trim();
    let sentence_end = [". ", ".\n"]
        .iter()
        .filter_map(|sep| trimmed.find(sep))
        .min();
    if let Some(idx) = sentence_end {
        trimmed[..=idx].to_string()
    } else if trimmed.ends_with('.') {
        trimmed.to_string()
    } else {
        trimmed.lines().next().unwrap_or("").to_string()
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_str_escapes_special_chars() {
        assert_eq!(json_str("hello"), "\"hello\"");
        assert_eq!(json_str("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_str("a\nb"), "\"a\\nb\"");
        assert_eq!(json_str("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_str("a\tb"), "\"a\\tb\"");
        assert_eq!(json_str("a\u{0001}b"), "\"a\\u0001b\"");
    }

    #[test]
    fn brief_extracts_first_sentence() {
        assert_eq!(brief(&None), "");
        assert_eq!(brief(&Some("Just one line".to_string())), "Just one line");
        assert_eq!(brief(&Some("First. Second.".to_string())), "First.");
        assert_eq!(brief(&Some("Trailing.".to_string())), "Trailing.");
        assert_eq!(brief(&Some("Line one\nLine two".to_string())), "Line one");
        // A paragraph break ends the first sentence even when a
        // `". "` boundary appears later.
        assert_eq!(
            brief(&Some("First.\n\nSecond one. Third.".to_string())),
            "First."
        );
    }
}
