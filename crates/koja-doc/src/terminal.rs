//! Terminal doc lookup backing `koja doc <symbol>` and `koja doc
//! search`. Matches against every doc-visible symbol and renders
//! plain markdown (no ANSI escapes) for terminal and AI consumption.
//! An exact name hit renders the full doc, anything else renders a
//! match list in roster order.

use std::ptr;

use crate::extract::{DocFunction, DocProject};
use crate::search::{Symbol, SymbolTarget, collect_symbols};

/// Result of a terminal doc search.
pub enum SearchOutcome {
    /// Rendered markdown to print, either a full doc page or a
    /// match list.
    Hits(String),
    /// Nothing matched the query.
    NoMatches,
}

/// Match `query` against every symbol in `project`, case-insensitive.
/// An exact hit on `Name`, `Owner.fn`, or their package-qualified
/// spellings renders the full doc. Several exact hits render a
/// disambiguation list. Otherwise substring hits on names, then on
/// doc bodies, render a match list.
pub fn search(project: &DocProject, query: &str) -> SearchOutcome {
    let symbols = collect_symbols(project);
    let needle = query.to_lowercase();

    match exact_hits(&symbols, &needle).as_slice() {
        [hit] => {
            let partials = partial_hits(&symbols, &needle, Some(hit));
            SearchOutcome::Hits(render_full(hit, &partials))
        }
        [] => {
            let partials = partial_hits(&symbols, &needle, None);
            if partials.is_empty() {
                return SearchOutcome::NoMatches;
            }
            let word = if partials.len() == 1 {
                "match"
            } else {
                "matches"
            };
            let header = format!("{} {word} for \"{query}\":", partials.len());
            SearchOutcome::Hits(render_list(&header, &partials))
        }
        exact => SearchOutcome::Hits(render_disambiguation(query, exact)),
    }
}

/// Exact-name lookup for `koja doc <symbol>`. One hit renders the
/// full doc, several render a disambiguation list, and no hit is
/// [`SearchOutcome::NoMatches`] with no substring fallback, so a
/// mistyped name does not turn into a long candidate list.
pub fn lookup(project: &DocProject, name: &str) -> SearchOutcome {
    let symbols = collect_symbols(project);
    let needle = name.to_lowercase();

    match exact_hits(&symbols, &needle).as_slice() {
        [] => SearchOutcome::NoMatches,
        [hit] => SearchOutcome::Hits(render_full(hit, &[])),
        exact => SearchOutcome::Hits(render_disambiguation(name, exact)),
    }
}

fn exact_hits<'a, 'p>(symbols: &'a [Symbol<'p>], needle: &str) -> Vec<&'a Symbol<'p>> {
    symbols
        .iter()
        .filter(|symbol| matches_exact(symbol, needle))
        .collect()
}

/// Substring hits in roster order, name matches first and then
/// symbols whose doc body mentions the needle. `exclude` drops the
/// exact hit the caller already rendered.
fn partial_hits<'a, 'p>(
    symbols: &'a [Symbol<'p>],
    needle: &str,
    exclude: Option<&Symbol<'p>>,
) -> Vec<&'a Symbol<'p>> {
    let candidates = symbols
        .iter()
        .filter(|symbol| !exclude.is_some_and(|hit| ptr::eq(*symbol, hit)));
    let by_name = candidates
        .clone()
        .filter(|symbol| matches_name(symbol, needle));
    let by_body =
        candidates.filter(|symbol| !matches_name(symbol, needle) && matches_body(symbol, needle));
    by_name.chain(by_body).collect()
}

fn render_disambiguation(query: &str, exact: &[&Symbol]) -> String {
    let header = format!(
        "\"{query}\" matches {} symbols. Use a qualified name:",
        exact.len()
    );
    render_list(&header, exact)
}

fn matches_exact(symbol: &Symbol, needle: &str) -> bool {
    let name = symbol.name.to_lowercase();
    let qualified = symbol.qualified_name().to_lowercase();
    name == needle
        || qualified == needle
        || name.split_once('/').is_some_and(|(base, _)| base == needle)
        || qualified
            .rsplit_once('/')
            .is_some_and(|(base, _)| base == needle)
}

fn matches_name(symbol: &Symbol, needle: &str) -> bool {
    symbol.qualified_name().to_lowercase().contains(needle)
}

fn matches_body(symbol: &Symbol, needle: &str) -> bool {
    symbol
        .doc()
        .as_deref()
        .is_some_and(|doc| doc.to_lowercase().contains(needle))
}

fn render_list(header: &str, symbols: &[&Symbol]) -> String {
    let mut out = format!("{header}\n\n");
    for symbol in symbols {
        out.push_str(&list_line(symbol));
        out.push('\n');
    }
    out
}

fn list_line(symbol: &Symbol) -> String {
    // Collapse wrapped doc lines so every list entry stays one line.
    let brief = symbol
        .brief()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let kind = if symbol.deprecated().is_some() {
        format!("{}, deprecated", symbol.kind)
    } else {
        symbol.kind.to_string()
    };
    if brief.is_empty() {
        format!("- {} ({kind})", symbol.qualified_name())
    } else {
        format!("- {} ({kind}): {brief}", symbol.qualified_name())
    }
}

fn render_full(hit: &Symbol, partials: &[&Symbol]) -> String {
    let mut out = format!("# {} ({})\n", header_name(hit), hit.kind);
    push_deprecation(&mut out, hit.deprecated());

    match &hit.target {
        SymbolTarget::Builtin(b) => {
            push_doc(&mut out, &b.doc);
            push_functions(&mut out, &b.functions);
        }
        SymbolTarget::Constant(_) => push_doc(&mut out, hit.doc()),
        SymbolTarget::Function(f) => {
            out.push_str("\n```koja\n");
            out.push_str(&f.signature_text());
            out.push_str("\n```\n");
            push_doc(&mut out, &f.doc);
        }
        SymbolTarget::Enum(e) => {
            push_doc(&mut out, &e.doc);
            push_list_section(&mut out, "Variants", &e.variants);
            push_functions(&mut out, &e.functions);
        }
        SymbolTarget::Protocol(p) => {
            push_doc(&mut out, &p.doc);
            push_functions(&mut out, &p.functions);
        }
        SymbolTarget::Struct(s) => {
            push_doc(&mut out, &s.doc);
            let fields: Vec<String> = s
                .fields
                .iter()
                .map(|f| match &f.default {
                    Some(default) => format!("{}: {} = {default}", f.name, f.type_name),
                    None => format!("{}: {}", f.name, f.type_name),
                })
                .collect();
            push_list_section(&mut out, "Fields", &fields);
            push_functions(&mut out, &s.functions);
        }
    }

    if !partials.is_empty() {
        out.push_str("\n## Also matched\n\n");
        for symbol in partials {
            out.push_str(&list_line(symbol));
            out.push('\n');
        }
    }
    out
}

/// Qualified name with generic parameters appended for the types
/// that carry them.
fn header_name(symbol: &Symbol) -> String {
    let type_params = match &symbol.target {
        SymbolTarget::Builtin(b) => &b.type_params,
        SymbolTarget::Protocol(p) => &p.type_params,
        SymbolTarget::Struct(s) => &s.type_params,
        _ => return symbol.qualified_name(),
    };
    if type_params.is_empty() {
        symbol.qualified_name()
    } else {
        format!("{}<{}>", symbol.qualified_name(), type_params.join(", "))
    }
}

fn push_doc(out: &mut String, doc: &Option<String>) {
    if let Some(doc) = doc {
        out.push('\n');
        out.push_str(doc.trim());
        out.push('\n');
    }
}

fn push_deprecation(out: &mut String, message: Option<&str>) {
    if let Some(message) = message {
        out.push_str("\n> **Deprecated**\n>\n");
        for line in message.trim().lines() {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
}

fn push_list_section(out: &mut String, title: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {title}\n\n"));
    for line in lines {
        out.push_str(&format!("- {line}\n"));
    }
}

fn push_functions(out: &mut String, functions: &[DocFunction]) {
    if functions.is_empty() {
        return;
    }
    out.push_str("\n## Functions\n");
    for f in functions {
        out.push_str(&format!("\n### `{}`\n", f.signature_text()));
        push_deprecation(out, f.deprecated.as_deref());
        push_doc(out, &f.doc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{
        DocBuiltin, DocConstant, DocEnum, DocField, DocParam, DocStruct, PackageKind,
    };

    fn hits(outcome: SearchOutcome) -> String {
        match outcome {
            SearchOutcome::Hits(text) => text,
            SearchOutcome::NoMatches => panic!("expected hits"),
        }
    }

    fn sample_project() -> DocProject {
        let mut project = DocProject::new("MyApp");
        let global = project.ensure_package("Global", PackageKind::Stdlib);
        global.builtins.push(DocBuiltin {
            deprecated: None,
            doc: Some(
                "A growable list. Backed by a heap block. Items may be optional.".to_string(),
            ),
            functions: vec![DocFunction {
                arity: 2,
                deprecated: None,
                doc: Some(
                    "Append an item.\n\n## Examples\n\n```koja\nlist.append(1)\n```".to_string(),
                ),
                error_type: None,
                name: "append".to_string(),
                params: vec![
                    DocParam {
                        name: "self".to_string(),
                        type_name: String::new(),
                    },
                    DocParam {
                        name: "item".to_string(),
                        type_name: "T".to_string(),
                    },
                ],
                return_type: Some("List<T>".to_string()),
                type_params: vec![],
            }],
            name: "List".to_string(),
            type_params: vec!["T".to_string()],
        });
        global.enums.push(DocEnum {
            deprecated: None,
            doc: Some("An optional value. `Config.port` reads one.".to_string()),
            functions: vec![],
            name: "Option".to_string(),
            variants: vec!["Some(T)".to_string(), "None".to_string()],
        });

        let app = project.ensure_package("MyApp", PackageKind::Project);
        app.structs.push(DocStruct {
            deprecated: None,
            doc: Some("Connection settings.".to_string()),
            fields: vec![DocField {
                default: Some("5432".to_string()),
                name: "port".to_string(),
                type_name: "Int".to_string(),
            }],
            functions: vec![],
            name: "Config".to_string(),
            type_params: vec![],
        });

        let json = project.ensure_package("JSON", PackageKind::Stdlib);
        json.constants.push(DocConstant {
            deprecated: None,
            doc: Some("Maximum nesting depth.".to_string()),
            name: "MAX_DEPTH".to_string(),
        });
        json.enums.push(DocEnum {
            deprecated: None,
            doc: None,
            functions: vec![],
            name: "Option".to_string(),
            variants: vec![],
        });
        project
    }

    #[test]
    fn deprecation_renders_as_a_multiline_blockquote() {
        let mut text = String::new();
        push_deprecation(
            &mut text,
            Some("Use `new_api` instead.\nRemoved in 0.19.0."),
        );
        assert_eq!(
            text,
            "\n> **Deprecated**\n>\n> Use `new_api` instead.\n> Removed in 0.19.0.\n"
        );
    }

    #[test]
    fn exact_builtin_hit_renders_full_doc() {
        let project = sample_project();
        let text = hits(search(&project, "list"));
        assert!(text.starts_with("# Global.List<T> (builtin)\n"));
        assert!(text.contains("A growable list."));
        assert!(text.contains("### `fn append(self, item: T) -> List<T>`"));
        assert!(text.contains("list.append(1)"));
        assert!(text.contains("## Also matched\n\n- Global.List.append/2 (fn): Append an item.\n"));
    }

    #[test]
    fn exact_struct_hit_renders_fields() {
        let project = sample_project();
        let text = hits(search(&project, "config"));
        assert!(text.starts_with("# MyApp.Config (struct)\n"));
        assert!(text.contains("Connection settings."));
        assert!(text.contains("## Fields\n\n- port: Int = 5432\n"));
    }

    #[test]
    fn exact_function_hit_renders_signature_and_doc() {
        let project = sample_project();
        let text = hits(search(&project, "List.append"));
        assert!(text.starts_with("# Global.List.append/2 (fn)\n"));
        assert!(text.contains("```koja\nfn append(self, item: T) -> List<T>\n```"));
        assert!(text.contains("Append an item."));
    }

    #[test]
    fn ambiguous_exact_hit_lists_qualified_names() {
        let project = sample_project();
        let text = hits(search(&project, "option"));
        assert!(text.starts_with("\"option\" matches 2 symbols."));
        assert!(text.contains("- Global.Option (enum): An optional value.\n"));
        assert!(text.contains("- JSON.Option (enum)\n"));
    }

    #[test]
    fn qualified_query_disambiguates() {
        let project = sample_project();
        let text = hits(search(&project, "json.option"));
        assert!(text.starts_with("# JSON.Option (enum)\n"));
    }

    #[test]
    fn partial_query_lists_matches() {
        let project = sample_project();
        let text = hits(search(&project, "appe"));
        assert!(text.starts_with("1 match for \"appe\":"));
        assert!(text.contains("- Global.List.append/2 (fn): Append an item.\n"));
    }

    #[test]
    fn no_match_reports_none() {
        let project = sample_project();
        assert!(matches!(search(&project, "zzz"), SearchOutcome::NoMatches));
    }

    #[test]
    fn body_only_query_lists_symbols_that_mention_it() {
        let project = sample_project();
        let text = hits(search(&project, "heap block"));
        assert_eq!(
            text,
            "1 match for \"heap block\":\n\n- Global.List (builtin): A growable list.\n"
        );
    }

    #[test]
    fn name_hits_come_before_body_hits() {
        let project = sample_project();
        let text = hits(search(&project, "opt"));
        assert_eq!(
            text,
            "3 matches for \"opt\":\n\n\
             - Global.Option (enum): An optional value.\n\
             - JSON.Option (enum)\n\
             - Global.List (builtin): A growable list.\n"
        );
    }

    #[test]
    fn exact_hit_also_lists_body_matches() {
        let project = sample_project();
        let text = hits(search(&project, "config"));
        assert!(text.starts_with("# MyApp.Config (struct)\n"));
        assert!(text.ends_with("## Also matched\n\n- Global.Option (enum): An optional value.\n"));
    }

    #[test]
    fn lookup_renders_exact_hit_without_trailer() {
        let project = sample_project();
        let text = hits(lookup(&project, "Config"));
        assert!(text.starts_with("# MyApp.Config (struct)\n"));
        assert!(!text.contains("## Also matched"));
    }

    #[test]
    fn lookup_disambiguates_but_never_falls_back_to_substrings() {
        let project = sample_project();
        let text = hits(lookup(&project, "option"));
        assert!(text.starts_with("\"option\" matches 2 symbols."));
        assert!(matches!(lookup(&project, "appe"), SearchOutcome::NoMatches));
        assert!(matches!(lookup(&project, "heap"), SearchOutcome::NoMatches));
    }
}
