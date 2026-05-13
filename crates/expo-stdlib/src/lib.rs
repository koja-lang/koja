//! Embedded standard library sources for the Expo language.
//!
//! Sources live in `expo/lib/` as proper Expo projects. The build script
//! discovers all `.expo` files and generates the constants, SOURCES array,
//! and QUALIFIED_MODULES list automatically.

use std::path::PathBuf;

use expo_parser::SourceFile;

include!(concat!(env!("OUT_DIR"), "/stdlib_gen.rs"));

/// Materialize [`ALPHA_AUTOIMPORT`] as parser-ready [`SourceFile`]s,
/// in `SOURCES` order. Each entry's package is the prefix of the
/// `Package.module` key (so `Global.time` lands in `"Global"`); the
/// `path` is a synthetic `<Package.module>` marker, matching the
/// convention noted on [`SourceFile::path`] for embedded sources.
///
/// Driver and alpha tests both call this and prepend the result to
/// the user's source list before invoking `parse_program` so every
/// alpha pipeline sees the curated stdlib subset without duplicating
/// the conversion logic. New entries are added by listing them in
/// `build.rs::alpha_modules`.
pub fn alpha_autoimport_sources() -> Vec<SourceFile> {
    sources_from_table(ALPHA_AUTOIMPORT)
}

/// Materialize [`ALPHA_QUALIFIED`] as parser-ready [`SourceFile`]s,
/// in declaration order. Mirrors [`alpha_autoimport_sources`] but
/// for qualified packages — those whose decls land in their own
/// package namespace (`Crypto.SHA256`, etc) and need an `alias` in
/// the user's source to be referenced unqualified.
///
/// Loaded alongside the autoimport set; alpha pipelines prepend
/// both before the user file so `validate_aliases` sees the target
/// packages already registered. Pragmatic stand-in for on-demand
/// `IRPackage` loading; the curated package list lives in
/// `build.rs::alpha_qualified_packages`.
pub fn alpha_qualified_sources() -> Vec<SourceFile> {
    sources_from_table(ALPHA_QUALIFIED)
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
