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
    ALPHA_AUTOIMPORT
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
