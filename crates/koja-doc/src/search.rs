//! Build the `search-index.json` payload powering the sidebar
//! fuzzy search. One entry per item (struct / enum / protocol /
//! top-level fn / constant) and one entry per method on a type
//! (deep-linking to `#fn-<method>` on the type's page). The JSON
//! is also the AI-friendly bundle: every doc-visible symbol in
//! the project + bundled stdlib + bundled deps is present with
//! its kind, owning package, URL, and brief.
//!
//! We hand-roll the JSON encoding to avoid an extra workspace
//! dependency on `serde_json`. The payload shape is fixed and
//! the only escaping concern is doc-string content.

use crate::extract::{DocPackage, DocProject};

/// Format `project` as the contents of `doc/search-index.json`,
/// ready to be written verbatim by the driver. Same sort order
/// the renderer uses for the per-package item lists, so an
/// alphabetical hit list matches the visible sidebar when the
/// search box is empty.
pub fn search_index_json(project: &DocProject) -> String {
    let mut entries: Vec<SearchEntry> = Vec::new();
    for pkg in &project.packages {
        collect_package_entries(pkg, &mut entries);
    }

    let mut out = String::from("[");
    for (idx, entry) in entries.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str("\n  {");
        out.push_str(&format!("\"pkg\":{},", json_str(&entry.pkg)));
        out.push_str(&format!("\"name\":{},", json_str(&entry.name)));
        out.push_str(&format!("\"kind\":{},", json_str(&entry.kind)));
        out.push_str(&format!("\"url\":{},", json_str(&entry.url)));
        out.push_str(&format!("\"brief\":{}", json_str(&entry.brief)));
        out.push('}');
    }
    if !entries.is_empty() {
        out.push('\n');
    }
    out.push(']');
    out.push('\n');
    out
}

struct SearchEntry {
    brief: String,
    kind: String,
    name: String,
    pkg: String,
    url: String,
}

fn collect_package_entries(pkg: &DocPackage, out: &mut Vec<SearchEntry>) {
    for c in &pkg.constants {
        out.push(SearchEntry {
            brief: brief(&c.doc),
            kind: "const".to_string(),
            name: c.name.clone(),
            pkg: pkg.name.clone(),
            url: format!("{}/{}.html", pkg.name, c.name),
        });
    }
    for e in &pkg.enums {
        out.push(SearchEntry {
            brief: brief(&e.doc),
            kind: "enum".to_string(),
            name: e.name.clone(),
            pkg: pkg.name.clone(),
            url: format!("{}/{}.html", pkg.name, e.name),
        });
        for f in &e.functions {
            out.push(method_entry(pkg, &e.name, f));
        }
    }
    for f in &pkg.functions {
        out.push(SearchEntry {
            brief: brief(&f.doc),
            kind: "fn".to_string(),
            name: f.name.clone(),
            pkg: pkg.name.clone(),
            url: format!("{}/{}.html", pkg.name, f.name),
        });
    }
    for p in &pkg.protocols {
        out.push(SearchEntry {
            brief: brief(&p.doc),
            kind: "protocol".to_string(),
            name: p.name.clone(),
            pkg: pkg.name.clone(),
            url: format!("{}/{}.html", pkg.name, p.name),
        });
        for f in &p.functions {
            out.push(method_entry(pkg, &p.name, f));
        }
    }
    for s in &pkg.structs {
        out.push(SearchEntry {
            brief: brief(&s.doc),
            kind: "struct".to_string(),
            name: s.name.clone(),
            pkg: pkg.name.clone(),
            url: format!("{}/{}.html", pkg.name, s.name),
        });
        for f in &s.functions {
            out.push(method_entry(pkg, &s.name, f));
        }
    }
}

fn method_entry(pkg: &DocPackage, owner: &str, f: &crate::extract::DocFunction) -> SearchEntry {
    SearchEntry {
        brief: brief(&f.doc),
        kind: "fn".to_string(),
        name: format!("{owner}.{}", f.name),
        pkg: pkg.name.clone(),
        url: format!("{}/{owner}.html#fn-{}", pkg.name, f.name),
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
    if let Some(idx) = trimmed.find(". ") {
        trimmed[..=idx].to_string()
    } else if let Some(idx) = trimmed.find(".\n") {
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
    }
}
