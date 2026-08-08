//! Embedded standard library sources for the Koja language.
//!
//! Sources live in `koja/lib/` as proper Koja projects. The build
//! script discovers all `.koja` files and generates the constants
//! plus the [`AUTOIMPORT`] and [`QUALIFIED`] tables.
//!
//! [`extract`] materializes the embedded sources on disk under
//! `~/.koja/stdlib/` so tooling (LSP navigation, agents grepping the
//! stdlib) can reach real files. The pipeline itself always compiles
//! from the embedded strings.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use koja_parser::SourceFile;

include!(concat!(env!("OUT_DIR"), "/stdlib_gen.rs"));

/// How many extraction directories [`extract`] keeps. Dev builds
/// churn out a new directory per stdlib edit, so prune by last use.
const KEPT_EXTRACTIONS: usize = 8;

/// Marker file whose mtime records an extraction's last use.
const USED_MARKER: &str = ".used";

/// Root of the per-user Koja directory: `KOJA_HOME` or `~/.koja`.
pub fn koja_home() -> Result<PathBuf, String> {
    if let Ok(home) = env::var("KOJA_HOME") {
        return Ok(PathBuf::from(home));
    }
    match env::var("HOME") {
        Ok(home) => Ok(PathBuf::from(home).join(".koja")),
        Err(_) => Err("cannot determine home directory (set KOJA_HOME or HOME)".to_string()),
    }
}

/// Directory name for this build's extraction. Content-keyed so a
/// dev build never overwrites a released compiler's extraction.
pub fn extraction_dir_name() -> String {
    format!("{}-{}", env!("CARGO_PKG_VERSION"), STDLIB_HASH8)
}

/// Materialize the embedded stdlib on disk and return its root.
/// Idempotent, so callers run it on every use and a pruned directory
/// heals on the next call.
pub fn extract() -> Result<PathBuf, String> {
    extract_into(&koja_home()?.join("stdlib"))
}

fn extract_into(cache_root: &Path) -> Result<PathBuf, String> {
    let target = cache_root.join(extraction_dir_name());
    if target.is_dir() {
        touch_used(&target);
        return Ok(target);
    }

    fs::create_dir_all(cache_root).map_err(|e| format!("{}: {e}", cache_root.display()))?;
    let staging = cache_root.join(format!(
        ".stage-{}-{}",
        extraction_dir_name(),
        process::id()
    ));
    write_files(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;

    // A concurrent extractor winning the rename race is success.
    if let Err(rename_error) = fs::rename(&staging, &target) {
        let _ = fs::remove_dir_all(&staging);
        if !target.is_dir() {
            return Err(format!("{}: {rename_error}", target.display()));
        }
    }
    touch_used(&target);
    prune(cache_root);
    Ok(target)
}

fn write_files(root: &Path) -> std::io::Result<()> {
    for (relative, contents) in FILES {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
    }
    fs::write(root.join(USED_MARKER), b"")?;
    Ok(())
}

fn touch_used(extraction: &Path) {
    let _ = fs::write(extraction.join(USED_MARKER), b"");
}

/// Delete all but the most recently used [`KEPT_EXTRACTIONS`]
/// directories. Best-effort, a wrongly pruned directory just
/// re-extracts on next use.
fn prune(cache_root: &Path) {
    let Ok(entries) = fs::read_dir(cache_root) else {
        return;
    };
    let mut extractions: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| (last_used(&e.path()), e.path()))
        .collect();
    extractions.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in extractions.into_iter().skip(KEPT_EXTRACTIONS) {
        let _ = fs::remove_dir_all(&path);
    }
}

fn last_used(extraction: &Path) -> std::time::SystemTime {
    fs::metadata(extraction.join(USED_MARKER))
        .or_else(|_| fs::metadata(extraction))
        .and_then(|m| m.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
}

/// Materialize [`AUTOIMPORT`] (the full `Global` package) as
/// parser-ready [`SourceFile`]s in declaration order. Each entry's
/// package is the prefix of the `Package.module` key (so
/// `Global.time` lands in `"Global"`), and the `path` is a synthetic
/// `<Package.module>` marker, matching the convention noted on
/// [`SourceFile::path`] for embedded sources.
///
/// Driver and tests both call this and prepend the result to the
/// user's source list before invoking `parse_program`, so every
/// pipeline run sees the stdlib without duplicating the conversion
/// logic.
pub fn autoimport_sources() -> Vec<SourceFile> {
    sources_from_table(AUTOIMPORT, None)
}

/// Materialize [`QUALIFIED`] as parser-ready [`SourceFile`]s in
/// declaration order. Mirrors [`autoimport_sources`] but for
/// qualified packages, those whose decls land in their own
/// package namespace (`Crypto.SHA256`, etc) and need an `alias` in
/// the user's source to be referenced unqualified.
///
/// Loaded alongside the autoimport set. Pipeline runs prepend both
/// before the user file so `validate_aliases` sees the target
/// packages already registered. Pragmatic stand-in for on-demand
/// `IRPackage` loading.
pub fn qualified_sources() -> Vec<SourceFile> {
    sources_from_table(QUALIFIED, None)
}

/// [`autoimport_sources`] with paths rooted at an [`extract`]ion, so
/// diagnostics and go-to-definition land in real files.
pub fn autoimport_sources_at(extraction_root: &Path) -> Vec<SourceFile> {
    sources_from_table(AUTOIMPORT, Some(extraction_root))
}

/// [`qualified_sources`] with paths rooted at an [`extract`]ion.
pub fn qualified_sources_at(extraction_root: &Path) -> Vec<SourceFile> {
    sources_from_table(QUALIFIED, Some(extraction_root))
}

fn sources_from_table(
    table: &[(&str, &str, &str)],
    extraction_root: Option<&Path>,
) -> Vec<SourceFile> {
    table
        .iter()
        .map(|(name, source, relative)| {
            let (package, _) = name.split_once('.').unwrap_or((name, ""));
            let path = match extraction_root {
                Some(root) => root.join(relative),
                None => PathBuf::from(format!("<{name}>")),
            };
            SourceFile {
                package: package.to_string(),
                path,
                source: (*source).to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache_root(name: &str) -> PathBuf {
        let root = env::temp_dir().join(format!("koja-stdlib-test-{name}-{}", process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn extraction_is_idempotent() {
        let cache_root = temp_cache_root("idempotent");
        let first = extract_into(&cache_root).unwrap();
        let sentinel = first.join("sentinel");
        fs::write(&sentinel, b"kept").unwrap();

        let second = extract_into(&cache_root).unwrap();
        assert_eq!(first, second);
        assert!(sentinel.exists(), "second call must not rewrite the dir");
        assert!(first.join(FILES[0].0).is_file());

        fs::remove_dir_all(&cache_root).unwrap();
    }

    #[test]
    fn extraction_dir_is_keyed_by_version_and_hash() {
        let name = extraction_dir_name();
        let (version, hash) = name.rsplit_once('-').unwrap();
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
        assert_eq!(hash.len(), 8);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn prune_keeps_most_recently_used() {
        let cache_root = temp_cache_root("prune");
        fs::create_dir_all(&cache_root).unwrap();
        for i in 0..KEPT_EXTRACTIONS + 3 {
            let dir = cache_root.join(format!("0.0.{i}-deadbeef"));
            fs::create_dir_all(&dir).unwrap();
            let marker = dir.join(USED_MARKER);
            fs::write(&marker, b"").unwrap();
            // Space the markers out so the LRU order is unambiguous.
            let mtime = std::time::SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(1_000_000 + i as u64);
            set_mtime(&marker, mtime);
        }

        prune(&cache_root);

        let survivors: Vec<String> = fs::read_dir(&cache_root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(survivors.len(), KEPT_EXTRACTIONS);
        assert!(!survivors.contains(&"0.0.0-deadbeef".to_string()));
        assert!(!survivors.contains(&"0.0.1-deadbeef".to_string()));
        assert!(!survivors.contains(&"0.0.2-deadbeef".to_string()));

        fs::remove_dir_all(&cache_root).unwrap();
    }

    fn set_mtime(path: &Path, time: std::time::SystemTime) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(time).unwrap();
    }
}
