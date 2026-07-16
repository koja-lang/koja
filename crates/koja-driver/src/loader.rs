//! File and project source discovery.
//!
//! Two layers:
//!
//! - [`walk_source_files`] is the lone recursive directory walk used
//!   across the driver (compiler, docs, formatter).
//! - [`ProjectLoader`] sits on top of it and resolves a manifest into
//!   package-tagged [`LoadedSource`]s, folding in dependency and stdlib
//!   sources according to [`LoadOptions`]. The compiler and docs
//!   generator use it. The formatter only needs the walk.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::deps;
use crate::project::ProjectConfig;

/// Recursively collect files under `dir` whose extension is in
/// `extensions`. Results are sorted for deterministic output, and an
/// unreadable directory yields an empty vec rather than an error so
/// callers degrade gracefully.
pub(crate) fn walk_source_files(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_into(dir, extensions, &mut out);
    out.sort();
    out
}

fn walk_into(dir: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_into(&path, extensions, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| extensions.contains(&ext))
        {
            out.push(path);
        }
    }
}

/// Where a [`LoadedSource`] came from in the dependency graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceOrigin {
    Dependency,
    Project,
    Stdlib,
}

/// A single source file resolved by [`ProjectLoader`], tagged with the
/// package it belongs to and its origin tier.
pub(crate) struct LoadedSource {
    pub origin: SourceOrigin,
    pub package: String,
    pub path: PathBuf,
    pub source: String,
}

/// How the loader reacts to a malformed dependency or unreadable file.
/// `Strict` (compiler) surfaces a hard error, while `Lenient` (docs) warns
/// and skips so a single bad dependency can't sink the whole run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorPolicy {
    Lenient,
    Strict,
}

/// Knobs for [`ProjectLoader::sources`]. Each field maps to a real
/// project-loading policy rather than an ad-hoc toggle.
pub(crate) struct LoadOptions {
    /// File extensions to collect (e.g. `&["koja"]`).
    pub extensions: &'static [&'static str],
    /// Resolve `[dependencies]` transitively and walk each package's
    /// `src`.
    pub include_dependencies: bool,
    /// Append the embedded stdlib sources (packages already present are
    /// skipped).
    pub include_stdlib: bool,
    /// Include the project's `test` directories alongside `src`.
    pub include_tests: bool,
    /// Strictness for missing deps and read failures.
    pub on_error: ErrorPolicy,
}

/// Manifest-aware source resolver: given a parsed `koja.toml` and its
/// root directory, produces the package-tagged sources a build or doc
/// run operates on.
pub(crate) struct ProjectLoader<'a> {
    config: &'a ProjectConfig,
    root: &'a Path,
}

impl<'a> ProjectLoader<'a> {
    pub fn new(config: &'a ProjectConfig, root: &'a Path) -> Self {
        Self { config, root }
    }

    /// Resolve project sources per `opts`: the project's own `src`
    /// (plus `test` when requested), then dependency sources, then
    /// stdlib. Bails with `Err` only under [`ErrorPolicy::Strict`];
    /// `Lenient` always returns `Ok`, warning past anything broken.
    pub fn sources(&self, opts: LoadOptions) -> Result<Vec<LoadedSource>, String> {
        let strict = opts.on_error == ErrorPolicy::Strict;

        let mut collection = Collection::default();
        collection.seen_packages.insert(self.config.name.clone());
        // The compiler injects `Global` via autoimport, so reserve the
        // name to flag a dependency that collides with it.
        if strict && self.config.name != "Global" {
            collection.seen_packages.insert("Global".to_string());
        }

        self.push_package(
            &self.config.name,
            &self.config.src,
            self.root,
            &opts,
            SourceOrigin::Project,
            &mut collection,
        )?;
        if opts.include_tests {
            self.push_package(
                &self.config.name,
                &self.config.test,
                self.root,
                &opts,
                SourceOrigin::Project,
                &mut collection,
            )?;
        }

        if opts.include_dependencies {
            self.push_dependencies(&opts, &mut collection)?;
        }

        if opts.include_stdlib {
            for source in stdlib_sources() {
                if !collection.seen_packages.contains(&source.package) {
                    collection.out.push(source);
                }
            }
        }

        Ok(collection.out)
    }

    /// Walk every `src_dir` under `package_root`, read each matching
    /// file, and push it onto `collection` tagged with `package` +
    /// `origin`. The collection's `seen_paths` keeps overlapping roots
    /// from double-counting a file.
    fn push_package(
        &self,
        package: &str,
        src_dirs: &[String],
        package_root: &Path,
        opts: &LoadOptions,
        origin: SourceOrigin,
        collection: &mut Collection,
    ) -> Result<(), String> {
        for src in src_dirs {
            let dir = package_root.join(src);
            if !dir.is_dir() {
                continue;
            }
            for path in walk_source_files(&dir, opts.extensions) {
                if !collection.seen_paths.insert(path.clone()) {
                    continue;
                }
                let source = match fs::read_to_string(&path) {
                    Ok(source) => source,
                    Err(err) => {
                        let message = format!("error reading {}: {err}", path.display());
                        if opts.on_error == ErrorPolicy::Strict {
                            return Err(message);
                        }
                        eprintln!("warning: {message}");
                        continue;
                    }
                };
                collection.out.push(LoadedSource {
                    origin,
                    package: package.to_string(),
                    path,
                    source,
                });
            }
        }
        Ok(())
    }

    /// Resolve the transitive dependency graph via [`deps`] (offline:
    /// lock verification plus cache materialization for git deps,
    /// in-place resolution for path deps) and push each package's
    /// `src`. Under `Strict` a resolution failure is an error. Under
    /// `Lenient` it's a warning that skips all dependencies.
    fn push_dependencies(
        &self,
        opts: &LoadOptions,
        collection: &mut Collection,
    ) -> Result<(), String> {
        let resolved = match deps::sync_project(self.config, self.root) {
            Ok(resolved) => resolved,
            Err(message) => {
                if opts.on_error == ErrorPolicy::Strict {
                    return Err(message);
                }
                eprintln!("warning: {message}, skipping dependencies");
                return Ok(());
            }
        };
        for dep in resolved {
            collection.seen_packages.insert(dep.name.clone());
            self.push_package(
                &dep.name,
                &dep.src,
                &dep.root,
                opts,
                SourceOrigin::Dependency,
                collection,
            )?;
        }
        Ok(())
    }
}

/// Mutable accumulator threaded through a single [`ProjectLoader::sources`]
/// run: the collected sources plus the dedup sets that keep a file or
/// package from being counted twice across the project / dependency /
/// stdlib passes.
#[derive(Default)]
struct Collection {
    out: Vec<LoadedSource>,
    seen_packages: BTreeSet<String>,
    seen_paths: BTreeSet<PathBuf>,
}

/// All embedded stdlib sources (`autoimport` + `qualified`) as
/// [`LoadedSource`]s tagged [`SourceOrigin::Stdlib`]. Callers that
/// already carry a package set (e.g. a project + its deps) filter out
/// duplicates themselves. [`ProjectLoader::sources`] does so to avoid
/// documenting a package twice when run from inside a stdlib package.
pub(crate) fn stdlib_sources() -> Vec<LoadedSource> {
    koja_stdlib::autoimport_sources()
        .into_iter()
        .chain(koja_stdlib::qualified_sources())
        .map(|src| LoadedSource {
            origin: SourceOrigin::Stdlib,
            package: src.package,
            path: src.path,
            source: src.source,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project;

    fn unique_temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "koja-loader-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn file_names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// Scaffold a project named `Main` with one `src` file, one `test`
    /// file, and a path dependency `Greeter` declaring `dep_name`.
    fn scaffold(root: &Path, dep_name: &str) {
        write(
            &root.join("koja.toml"),
            "[project]\nname = \"Main\"\nversion = \"0.1.0\"\n\n[dependencies]\ngreeter = { path = \"libs/greeter\" }\n",
        );
        write(&root.join("src/main.koja"), "// main\n");
        write(&root.join("test/main_test.koja"), "// test\n");
        write(
            &root.join("libs/greeter/koja.toml"),
            &format!("[project]\nname = \"{dep_name}\"\nversion = \"0.1.0\"\n"),
        );
        write(&root.join("libs/greeter/src/greeter.koja"), "// greeter\n");
    }

    fn strict(include_tests: bool) -> LoadOptions {
        LoadOptions {
            extensions: &["koja"],
            include_dependencies: true,
            include_stdlib: false,
            include_tests,
            on_error: ErrorPolicy::Strict,
        }
    }

    #[test]
    fn walk_filters_by_extension_and_sorts() {
        let root = unique_temp("walk");
        write(&root.join("a.koja"), "");
        write(&root.join("b.kojs"), "");
        write(&root.join("c.txt"), "");
        write(&root.join("sub/d.koja"), "");

        assert_eq!(
            file_names(&walk_source_files(&root, &["koja"])),
            ["a.koja", "d.koja"]
        );
        assert_eq!(
            file_names(&walk_source_files(&root, &["koja", "kojs"])),
            ["a.koja", "b.kojs", "d.koja"]
        );
        assert!(walk_source_files(&root.join("missing"), &["koja"]).is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sources_tags_project_and_dependency() {
        let root = unique_temp("sources");
        scaffold(&root, "Greeter");
        let config = project::load_project(&root).unwrap().unwrap();

        let loaded = ProjectLoader::new(&config, &root)
            .sources(strict(false))
            .unwrap();
        let mut tagged: Vec<_> = loaded
            .iter()
            .map(|s| {
                (
                    s.path.file_name().unwrap().to_string_lossy().into_owned(),
                    s.origin,
                    s.package.clone(),
                )
            })
            .collect();
        tagged.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(
            tagged,
            vec![
                (
                    "greeter.koja".to_string(),
                    SourceOrigin::Dependency,
                    "Greeter".to_string()
                ),
                (
                    "main.koja".to_string(),
                    SourceOrigin::Project,
                    "Main".to_string()
                ),
            ]
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn include_tests_adds_test_dir() {
        let root = unique_temp("tests");
        scaffold(&root, "Greeter");
        let config = project::load_project(&root).unwrap().unwrap();

        let without = ProjectLoader::new(&config, &root)
            .sources(strict(false))
            .unwrap();
        assert!(!without.iter().any(|s| s.path.ends_with("main_test.koja")));

        let with = ProjectLoader::new(&config, &root)
            .sources(strict(true))
            .unwrap();
        assert!(
            with.iter()
                .any(|s| s.path.ends_with("main_test.koja") && s.origin == SourceOrigin::Project)
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn strict_rejects_duplicate_package_name() {
        let root = unique_temp("dup");
        scaffold(&root, "Main");
        let config = project::load_project(&root).unwrap().unwrap();

        let result = ProjectLoader::new(&config, &root).sources(strict(false));
        assert!(result.is_err());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn lenient_skips_broken_dependency() {
        let root = unique_temp("lenient");
        write(
            &root.join("koja.toml"),
            "[project]\nname = \"Main\"\nversion = \"0.1.0\"\n\n[dependencies]\nghost = { path = \"libs/ghost\" }\n",
        );
        write(&root.join("src/main.koja"), "// main\n");
        let config = project::load_project(&root).unwrap().unwrap();

        let loaded = ProjectLoader::new(&config, &root)
            .sources(LoadOptions {
                extensions: &["koja"],
                include_dependencies: true,
                include_stdlib: false,
                include_tests: false,
                on_error: ErrorPolicy::Lenient,
            })
            .unwrap();
        assert_eq!(
            file_names(&loaded.iter().map(|s| s.path.clone()).collect::<Vec<_>>()),
            ["main.koja"]
        );

        fs::remove_dir_all(&root).ok();
    }
}
