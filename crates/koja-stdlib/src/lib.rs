//! Embedded standard library sources for the Koja language.
//!
//! Sources live in `expo/lib/` as proper Koja projects. The build
//! script discovers all `.koja` files and generates the constants
//! plus the curated [`AUTOIMPORT`] and [`QUALIFIED`] tables.

use std::path::PathBuf;

use koja_parser::SourceFile;

include!(concat!(env!("OUT_DIR"), "/stdlib_gen.rs"));

/// Materialize [`AUTOIMPORT`] as parser-ready [`SourceFile`]s in
/// declaration order. Each entry's package is the prefix of the
/// `Package.module` key (so `Global.time` lands in `"Global"`); the
/// `path` is a synthetic `<Package.module>` marker, matching the
/// convention noted on [`SourceFile::path`] for embedded sources.
///
/// Driver and tests both call this and prepend the result to the
/// user's source list before invoking `parse_program`, so every
/// pipeline run sees the curated stdlib subset without duplicating
/// the conversion logic. New entries are added by listing them in
/// `build.rs::autoimport_modules`.
pub fn autoimport_sources() -> Vec<SourceFile> {
    sources_from_table(AUTOIMPORT)
}

/// Materialize [`QUALIFIED`] as parser-ready [`SourceFile`]s in
/// declaration order. Mirrors [`autoimport_sources`] but for
/// qualified packages — those whose decls land in their own
/// package namespace (`Crypto.SHA256`, etc) and need an `alias` in
/// the user's source to be referenced unqualified.
///
/// Loaded alongside the autoimport set; pipeline runs prepend both
/// before the user file so `validate_aliases` sees the target
/// packages already registered. Pragmatic stand-in for on-demand
/// `IRPackage` loading; the curated package list lives in
/// `build.rs::qualified_packages`.
pub fn qualified_sources() -> Vec<SourceFile> {
    sources_from_table(QUALIFIED)
}

fn sources_from_table(table: &[(&str, &str)]) -> Vec<SourceFile> {
    table
        .iter()
        .map(|(name, source)| {
            let (package, _) = name.split_once('.').unwrap_or((name, ""));
            SourceFile {
                package: package.to_string(),
                path: PathBuf::from(format!("<{name}>")),
                source: (*source).to_string(),
            }
        })
        .collect()
}
