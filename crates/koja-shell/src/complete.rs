//! Tab completion for the REPL.
//!
//! [`ShellHelper`] plugs a [`CompletionContext`] into rustyline. The
//! context is a snapshot of the most recent successful pipeline run:
//! the sealed [`GlobalRegistry`] plus the session fragment's
//! top-level bindings (`name -> ResolvedType`). Candidate
//! enumeration itself lives on the registry, shared with the LSP.
//! This module owns the REPL-side line analysis: what word is under
//! the cursor, whether it is a `:command`, a bare prefix, or a
//! dotted receiver, and how the receiver resolves (session binding
//! vs type name vs package).

use std::collections::HashMap;
use std::path::Path;

use koja_ast::ast::Statement;
use koja_ast::identifier::{GlobalRegistryId, Identifier, ResolvedType};
use koja_typecheck::{CheckedProgram, GlobalRegistry, KEYWORDS};
use rustyline::completion::{Completer, Pair};
use rustyline::{Context, Helper, Highlighter, Hinter, Validator};

use crate::COMMANDS;

/// What the completer sees of the session: the registry from the
/// last successful check, the package the session evaluates in, and
/// the session's top-level bindings with their resolved types.
pub(crate) struct CompletionContext {
    bindings: HashMap<String, ResolvedType>,
    package: String,
    registry: GlobalRegistry,
}

impl CompletionContext {
    /// Fallback when no successful check exists yet, e.g. a project
    /// baseline that fails to compile. Keywords and `:commands`
    /// still complete, symbols do not.
    pub(crate) fn empty(package: String) -> Self {
        Self {
            bindings: HashMap::new(),
            package,
            registry: GlobalRegistry::new(),
        }
    }

    /// Snapshot a successful check: clone the registry and collect
    /// the fragment file's top-level `name = expr` bindings with the
    /// resolved type the typechecker stamped on each initializer.
    /// Later rebindings of a name overwrite earlier ones.
    pub(crate) fn of(checked: &CheckedProgram, package: String, fragment_path: &Path) -> Self {
        let mut bindings = HashMap::new();
        let fragment_body = checked
            .packages
            .iter()
            .flat_map(|pkg| &pkg.files)
            .find(|file| file.path.as_deref() == Some(fragment_path))
            .and_then(|file| file.body.as_ref());
        for statement in fragment_body.into_iter().flatten() {
            if let Statement::Assignment { target, value, .. } = statement
                && target.segments.len() == 1
            {
                bindings.insert(target.segments[0].clone(), value.resolution.clone());
            }
        }
        Self {
            bindings,
            package,
            registry: checked.registry.clone(),
        }
    }

    /// Completions for the cursor at byte offset `pos` in `line`.
    /// Returns the offset the replacement starts at plus the sorted,
    /// deduplicated labels.
    pub(crate) fn candidates(&self, line: &str, pos: usize) -> (usize, Vec<String>) {
        let before = &line[..pos];
        if let Some(command) = command_prefix(before) {
            let labels = COMMANDS
                .iter()
                .filter(|c| c.starts_with(command))
                .map(ToString::to_string)
                .collect();
            return (before.len() - command.len(), finish(labels));
        }
        let start = word_start(before);
        let word = &before[start..];
        match word.rsplit_once('.') {
            Some((receiver, partial)) => (
                start + receiver.len() + 1,
                self.dot_labels(receiver, partial),
            ),
            None => (start, self.bare_labels(word)),
        }
    }

    /// Candidates for a bare (dotless) prefix: keywords, session
    /// bindings, symbols in the session package + `Global`, and
    /// package names usable as qualifiers.
    fn bare_labels(&self, prefix: &str) -> Vec<String> {
        let prefix_lower = prefix.to_ascii_lowercase();
        let mut labels: Vec<String> = KEYWORDS
            .iter()
            .filter(|kw| matches_prefix(kw, &prefix_lower))
            .map(ToString::to_string)
            .collect();
        labels.extend(
            self.bindings
                .keys()
                .filter(|name| matches_prefix(name, &prefix_lower))
                .cloned(),
        );
        let mut packages = vec![self.package.as_str()];
        if self.package != "Global" {
            packages.push("Global");
        }
        for pkg in packages {
            labels.extend(
                self.registry
                    .symbol_candidates(pkg, &self.package)
                    .iter()
                    .filter(|candidate| matches_prefix(candidate.label, &prefix_lower))
                    .map(|candidate| candidate.label.to_string()),
            );
        }
        labels.extend(
            self.registry
                .iter()
                .map(|(_, entry)| entry.identifier.package())
                .filter(|pkg| *pkg != "Global" && matches_prefix(pkg, &prefix_lower))
                .map(ToString::to_string),
        );
        finish(labels)
    }

    /// Candidates after a dot: instance members when `receiver` is a
    /// session binding, static members when it names a type
    /// (unqualified or `Package.Type`), and package symbols when it
    /// names a package.
    fn dot_labels(&self, receiver: &str, partial: &str) -> Vec<String> {
        let candidates = if let Some(type_id) = self.binding_type_id(receiver) {
            self.registry.dot_candidates(type_id, false)
        } else if let Some(type_id) = self.lookup_type(receiver) {
            self.registry.dot_candidates(type_id, true)
        } else if self.is_package(receiver) {
            self.registry.symbol_candidates(receiver, &self.package)
        } else {
            Vec::new()
        };
        let partial_lower = partial.to_ascii_lowercase();
        let labels = candidates
            .iter()
            .filter(|candidate| matches_prefix(candidate.label, &partial_lower))
            .map(|candidate| candidate.label.to_string())
            .collect();
        finish(labels)
    }

    /// Head type id of the session binding `name`, if any.
    fn binding_type_id(&self, name: &str) -> Option<GlobalRegistryId> {
        self.registry.head_type_id(self.bindings.get(name)?)
    }

    /// Resolve a dotted receiver as a type name: unqualified in the
    /// session package, then `Global`, then as `Package.Type`.
    fn lookup_type(&self, receiver: &str) -> Option<GlobalRegistryId> {
        let segments: Vec<String> = receiver.split('.').map(ToString::to_string).collect();
        let mut identifiers = vec![
            Identifier::new(&self.package, segments.clone()),
            Identifier::new("Global", segments.clone()),
        ];
        if segments.len() >= 2 {
            identifiers.push(Identifier::new(&segments[0], segments[1..].to_vec()));
        }
        identifiers
            .iter()
            .find_map(|identifier| self.registry.lookup(identifier).map(|(id, _)| id))
    }

    fn is_package(&self, name: &str) -> bool {
        !name.contains('.')
            && self
                .registry
                .iter()
                .any(|(_, entry)| entry.identifier.is_in_package(name))
    }
}

/// rustyline helper wiring [`CompletionContext`] into Tab. The
/// derived `Hinter` / `Highlighter` / `Validator` impls are no-ops.
#[derive(Helper, Highlighter, Hinter, Validator)]
pub(crate) struct ShellHelper {
    context: CompletionContext,
}

impl ShellHelper {
    pub(crate) fn new(context: CompletionContext) -> Self {
        Self { context }
    }

    /// Swap in the snapshot from the latest successful eval.
    pub(crate) fn set_context(&mut self, context: CompletionContext) {
        self.context = context;
    }
}

impl Completer for ShellHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let (start, labels) = self.context.candidates(line, pos);
        let pairs = labels
            .into_iter()
            .map(|label| Pair {
                display: label.clone(),
                replacement: label,
            })
            .collect();
        Ok((start, pairs))
    }
}

/// The in-flight `:command` prefix, when `before` is nothing but one.
fn command_prefix(before: &str) -> Option<&str> {
    let trimmed = before.trim_start();
    (trimmed.starts_with(':') && !trimmed.contains(char::is_whitespace)).then_some(trimmed)
}

/// Sort and deduplicate candidate labels for stable presentation.
fn finish(mut labels: Vec<String>) -> Vec<String> {
    labels.sort();
    labels.dedup();
    labels
}

/// Case-insensitive prefix match. The empty prefix matches everything.
fn matches_prefix(name: &str, prefix_lower: &str) -> bool {
    prefix_lower.is_empty() || name.to_ascii_lowercase().starts_with(prefix_lower)
}

/// Byte offset where the dotted word ending at `before`'s end starts.
fn word_start(before: &str) -> usize {
    let mut start = before.len();
    for (idx, ch) in before.char_indices().rev() {
        if !ch.is_alphanumeric() && ch != '_' && ch != '.' {
            break;
        }
        start = idx;
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{baseline_with_project, fragment_sources};
    use crate::{SESSION_PACKAGE, check_fragment};

    /// Context built the way [`crate::Session`] does: baseline plus
    /// the session `fragment`, checked, then snapshotted.
    fn context(package: &str, fragment: &str) -> CompletionContext {
        let baseline = baseline_with_project();
        let (sources, path) = fragment_sources(&baseline, package, fragment);
        let checked = check_fragment(sources, &path, false).expect("fragment should check");
        CompletionContext::of(&checked, package.to_string(), &path)
    }

    /// Labels for a cursor at the end of `line`.
    fn labels(context: &CompletionContext, line: &str) -> Vec<String> {
        context.candidates(line, line.len()).1
    }

    #[test]
    fn bare_prefix_finds_project_type() {
        let ctx = context("Demo", "");
        assert!(labels(&ctx, "Ca").contains(&"Calc".to_string()));
    }

    #[test]
    fn bare_prefix_finds_stdlib_symbol_and_keyword() {
        let ctx = context("Demo", "");
        assert!(labels(&ctx, "IO").contains(&"IO".to_string()));
        assert!(labels(&ctx, "whi").contains(&"while".to_string()));
    }

    #[test]
    fn bare_prefix_includes_session_bindings() {
        let ctx = context("Demo", "position = Point{x: 1, y: 2}");
        assert!(labels(&ctx, "pos").contains(&"position".to_string()));
    }

    #[test]
    fn static_dot_lists_static_methods() {
        let ctx = context("Demo", "");
        let (start, found) = ctx.candidates("Calc.", 5);
        assert_eq!(start, 5);
        assert!(found.contains(&"double".to_string()));
    }

    #[test]
    fn static_dot_on_enum_lists_variants() {
        let ctx = context("Demo", "");
        let found = labels(&ctx, "Color.");
        for variant in ["Blue", "Green", "Red"] {
            assert!(found.contains(&variant.to_string()), "missing {variant}");
        }
    }

    #[test]
    fn instance_dot_on_binding_lists_fields() {
        let ctx = context("Demo", "p = Point{x: 1, y: 2}");
        let found = labels(&ctx, "p.");
        assert!(found.contains(&"x".to_string()));
        assert!(found.contains(&"y".to_string()));
    }

    #[test]
    fn qualified_type_dot_resolves_across_packages() {
        let ctx = context(SESSION_PACKAGE, "");
        assert!(labels(&ctx, "Demo.Calc.dou").contains(&"double".to_string()));
    }

    #[test]
    fn package_dot_lists_package_symbols() {
        let ctx = context(SESSION_PACKAGE, "");
        let found = labels(&ctx, "Demo.");
        for symbol in ["Calc", "Color", "Point"] {
            assert!(found.contains(&symbol.to_string()), "missing {symbol}");
        }
    }

    #[test]
    fn command_prefix_completes_commands() {
        let ctx = context(SESSION_PACKAGE, "");
        assert_eq!(ctx.candidates(":re", 3), (0, vec![":reset".to_string()]));
    }
}
