//! File and project source discovery.
//!
//! Two layers:
//!
//! - [`walk_source_files`] is the lone recursive directory walk used
//!   across the toolchain (compiler, docs, formatter, editor).
//! - [`ProjectLoader`] sits on top of it and resolves a manifest into
//!   package-tagged [`LoadedSource`]s, folding in dependency and stdlib
//!   sources according to [`LoadOptions`]. The compiler, the docs
//!   generator, the shell, and the language server use it. The
//!   formatter only needs the walk.
//!
//! [`stdlib_sources`] is the one rule for which stdlib packages join
//! a bundle. The loader applies it when [`LoadOptions::stdlib`] is
//! set, and single-file callers apply it with an empty claim set.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::{fs, io};

use crate::deps;
use crate::manifest::ProjectConfig;

/// Recursively collect files under `dir` whose extension is in
/// `extensions`. Results are sorted for deterministic output, and an
/// unreadable directory yields an empty vec rather than an error so
/// callers degrade gracefully.
pub fn walk_source_files(dir: &Path, extensions: &[&str]) -> Vec<PathBuf> {
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
pub enum SourceOrigin {
    Dependency,
    Project,
    Stdlib,
}

/// A single source file resolved by [`ProjectLoader`], tagged with the
/// package it belongs to and its origin tier.
pub struct LoadedSource {
    pub origin: SourceOrigin,
    pub package: String,
    pub path: PathBuf,
    pub source: String,
}

/// How the loader reacts to a malformed dependency or unreadable file.
/// `Strict` (compiler) surfaces a hard error, while `Lenient` (docs,
/// shell, editor) records a warning in [`Loaded::warnings`] and skips,
/// so a single bad dependency cannot sink the whole run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorPolicy {
    Lenient,
    Strict,
}

/// How the loader treats `[dependencies]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dependencies {
    /// Read `deps/` as it is. Path deps resolve in place. A git dep
    /// that is not pinned or not materialized is skipped and named in
    /// [`Loaded::warnings`]. Never fetches, materializes, or writes.
    /// For editors.
    OnDisk,
    /// Leave dependencies out.
    Skip,
    /// Offline sync. Verify the lock and re-materialize stale deps
    /// from the cache. For compiles.
    Sync,
}

/// Which stdlib sources join a bundle. See [`stdlib_sources`].
pub struct StdlibOptions {
    /// Root of a [`koja_stdlib::extract`]ion, so stdlib paths are real
    /// files that diagnostics and go-to-definition can open. `None`
    /// uses the synthetic `<Package.module>` paths.
    pub extraction_root: Option<PathBuf>,
    /// Link the `Test` package. The compiler sets this exactly when it
    /// loads test sources, so `assert` cannot type check in
    /// application code.
    pub link_tests: bool,
}

/// Knobs for [`ProjectLoader::sources`]. Each field maps to a real
/// project-loading policy rather than an ad-hoc toggle.
pub struct LoadOptions {
    pub dependencies: Dependencies,
    /// File extensions to collect (e.g. `&["koja"]`).
    pub extensions: &'static [&'static str],
    /// Include the project's `test` directories alongside `src`.
    pub include_tests: bool,
    /// Strictness for missing deps and read failures.
    pub on_error: ErrorPolicy,
    /// Lead the bundle with the stdlib packages the project does not
    /// claim. `None` leaves the stdlib out.
    pub stdlib: Option<StdlibOptions>,
}

/// What a load produced. The sources come in bundle order, stdlib
/// first, then the project, then its dependencies. The warnings are
/// what a [`ErrorPolicy::Lenient`] load collected, and a `Strict` load
/// never has any.
#[derive(Default)]
pub struct Loaded {
    /// Namespaces of the packages whose manifest set
    /// `experimental = true`. The project is in the set when its own
    /// manifest did, and each dependency is in the set when its
    /// manifest did. The typechecker silences experimental warnings
    /// inside these packages only.
    pub experimental_packages: BTreeSet<String>,
    pub sources: Vec<LoadedSource>,
    pub warnings: Vec<String>,
}

/// Manifest-aware source resolver: given a parsed `koja.toml` and its
/// root directory, produces the package-tagged sources a build, doc
/// run, or editor session operates on.
pub struct ProjectLoader<'a> {
    config: &'a ProjectConfig,
    overlays: Option<&'a HashMap<PathBuf, String>>,
    root: &'a Path,
}

impl<'a> ProjectLoader<'a> {
    pub fn new(config: &'a ProjectConfig, root: &'a Path) -> Self {
        Self {
            config,
            overlays: None,
            root,
        }
    }

    /// Read the files `overlays` names from it instead of from disk.
    /// Keys are canonical paths. Editors pass their open buffers so
    /// unsaved edits take part in the load.
    pub fn overlays(mut self, overlays: &'a HashMap<PathBuf, String>) -> Self {
        self.overlays = Some(overlays);
        self
    }

    /// Resolve project sources per `opts`. Stdlib comes first, then
    /// the project's own `src` (plus `test` when requested), then
    /// dependency sources. Bails with `Err` only under
    /// [`ErrorPolicy::Strict`]. `Lenient` always returns `Ok`, with
    /// anything broken named in [`Loaded::warnings`].
    pub fn sources(&self, opts: LoadOptions) -> Result<Loaded, String> {
        let namespace = self.config.namespace();

        let mut collection = Collection::default();
        collection.claimed.insert(namespace.clone());
        if self.config.experimental {
            collection.experimental.insert(namespace.clone());
        }

        self.push_package(
            &namespace,
            &self.config.src,
            self.root,
            &opts,
            SourceOrigin::Project,
            &mut collection,
        )?;
        if opts.include_tests {
            self.push_package(
                &namespace,
                &self.config.test,
                self.root,
                &opts,
                SourceOrigin::Project,
                &mut collection,
            )?;
        }

        if opts.dependencies != Dependencies::Skip {
            self.push_dependencies(&opts, &mut collection)?;
        }

        let mut sources = match &opts.stdlib {
            Some(stdlib) => stdlib_sources(&collection.claimed, stdlib),
            None => Vec::new(),
        };
        sources.append(&mut collection.out);
        Ok(Loaded {
            experimental_packages: collection.experimental,
            sources,
            warnings: collection.warnings,
        })
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
                let source = match self.text_of(&path) {
                    Ok(source) => source,
                    Err(err) => {
                        let message = format!("error reading {}: {err}", path.display());
                        if opts.on_error == ErrorPolicy::Strict {
                            return Err(message);
                        }
                        collection.warnings.push(message);
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

    /// The text of `path`, from the overlay that names it or else
    /// from disk.
    fn text_of(&self, path: &Path) -> io::Result<String> {
        if let Some(overlays) = self.overlays
            && let Some(text) = overlays.get(path).or_else(|| {
                let canonical = path.canonicalize().ok()?;
                overlays.get(&canonical)
            })
        {
            return Ok(text.clone());
        }
        fs::read_to_string(path)
    }

    /// Resolve the transitive dependency graph via [`deps`] in the
    /// mode `opts` asks for and push each package's `src`. Under
    /// `Strict` a resolution failure is an error. Under `Lenient` it
    /// is a warning that skips all dependencies.
    fn push_dependencies(
        &self,
        opts: &LoadOptions,
        collection: &mut Collection,
    ) -> Result<(), String> {
        let resolved = match opts.dependencies {
            Dependencies::Skip => return Ok(()),
            Dependencies::Sync => deps::sync_project(self.config, self.root),
            Dependencies::OnDisk => deps::resolve_on_disk(self.config, self.root).map(|on_disk| {
                collection.warnings.extend(on_disk.warnings);
                on_disk.resolved
            }),
        };
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(message) => {
                if opts.on_error == ErrorPolicy::Strict {
                    return Err(message);
                }
                collection
                    .warnings
                    .push(format!("{message}, skipping dependencies"));
                return Ok(());
            }
        };
        for dep in resolved {
            collection.claimed.insert(dep.namespace.clone());
            if dep.experimental {
                collection.experimental.insert(dep.namespace.clone());
            }
            self.push_package(
                &dep.namespace,
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
/// run. It holds the project and dependency sources, the packages
/// they claim (which the stdlib rule leaves out), the packages that
/// opted in to experimental declarations, the paths already seen (so
/// overlapping roots do not double-count a file), and the warnings a
/// `Lenient` load collects.
#[derive(Default)]
struct Collection {
    claimed: BTreeSet<String>,
    experimental: BTreeSet<String>,
    out: Vec<LoadedSource>,
    seen_paths: BTreeSet<PathBuf>,
    warnings: Vec<String>,
}

/// The stdlib sources a bundle links, as [`SourceOrigin::Stdlib`]
/// [`LoadedSource`]s in link order, `Global` first and the qualified
/// packages after it. This is the one rule for every caller. The
/// loader applies it with the packages the project and its
/// dependencies claim, and a single-file compile applies it with an
/// empty set.
///
/// A stdlib package in `claimed` is left out, so a stdlib self-compile
/// (`lib/global`, `lib/json`, and so on) does not define its
/// declarations twice.
///
/// The qualified packages ship typechecked against the published
/// `Global`. When the project is `Global` itself, its edited sources
/// and the baked qualified packages disagree, so the qualified set
/// stays out. A test build of `Global` is the exception, because the
/// harness needs `Test` and `Test` needs `JSON`.
pub fn stdlib_sources(claimed: &BTreeSet<String>, opts: &StdlibOptions) -> Vec<LoadedSource> {
    let (autoimport, qualified) = match &opts.extraction_root {
        Some(root) => (
            koja_stdlib::autoimport_sources_at(root),
            koja_stdlib::qualified_sources_at(root),
        ),
        None => (
            koja_stdlib::autoimport_sources(),
            koja_stdlib::qualified_sources(),
        ),
    };
    let editing_global = claimed.contains("Global");
    let links = |package: &str| {
        if claimed.contains(package) {
            false
        } else if editing_global {
            opts.link_tests && koja_stdlib::TEST_BUILD_PACKAGES.contains(&package)
        } else {
            opts.link_tests || package != koja_stdlib::TEST_PACKAGE
        }
    };
    autoimport
        .into_iter()
        .chain(qualified)
        .filter(|src| links(&src.package))
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
    use crate::manifest;

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

    /// Scaffold a project named `main` with one `src` file, one `test`
    /// file, and a path dependency `greeter` declaring `dep_name`.
    fn scaffold(root: &Path, dep_name: &str) {
        write(
            &root.join("koja.toml"),
            "[project]\nname = \"main\"\nversion = \"0.1.0\"\n\n[dependencies]\ngreeter = { path = \"libs/greeter\" }\n",
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
            dependencies: Dependencies::Sync,
            extensions: &["koja"],
            include_tests,
            on_error: ErrorPolicy::Strict,
            stdlib: None,
        }
    }

    fn lenient(dependencies: Dependencies) -> LoadOptions {
        LoadOptions {
            dependencies,
            extensions: &["koja"],
            include_tests: false,
            on_error: ErrorPolicy::Lenient,
            stdlib: None,
        }
    }

    fn packages(sources: &[LoadedSource]) -> Vec<&str> {
        let mut seen = Vec::new();
        for source in sources {
            if !seen.contains(&source.package.as_str()) {
                seen.push(source.package.as_str());
            }
        }
        seen
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
        scaffold(&root, "greeter");
        let config = manifest::load_project(&root).unwrap().unwrap();

        let loaded = ProjectLoader::new(&config, &root)
            .sources(strict(false))
            .unwrap();
        assert!(loaded.warnings.is_empty());
        let mut tagged: Vec<_> = loaded
            .sources
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
        scaffold(&root, "greeter");
        let config = manifest::load_project(&root).unwrap().unwrap();

        let without = ProjectLoader::new(&config, &root)
            .sources(strict(false))
            .unwrap();
        assert!(
            !without
                .sources
                .iter()
                .any(|s| s.path.ends_with("main_test.koja"))
        );

        let with = ProjectLoader::new(&config, &root)
            .sources(strict(true))
            .unwrap();
        assert!(
            with.sources
                .iter()
                .any(|s| s.path.ends_with("main_test.koja") && s.origin == SourceOrigin::Project)
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn experimental_packages_collects_each_opted_in_manifest() {
        let root = unique_temp("experimental");
        write(
            &root.join("koja.toml"),
            "[project]\nname = \"main\"\nversion = \"0.1.0\"\nexperimental = true\n\n\
             [dependencies]\nopted = { path = \"libs/opted\" }\nplain = { path = \"libs/plain\" }\n",
        );
        write(&root.join("src/main.koja"), "// main\n");
        write(
            &root.join("libs/opted/koja.toml"),
            "[project]\nname = \"opted\"\nversion = \"0.1.0\"\nexperimental = true\n",
        );
        write(&root.join("libs/opted/src/opted.koja"), "// opted\n");
        write(
            &root.join("libs/plain/koja.toml"),
            "[project]\nname = \"plain\"\nversion = \"0.1.0\"\n",
        );
        write(&root.join("libs/plain/src/plain.koja"), "// plain\n");
        let config = manifest::load_project(&root).unwrap().unwrap();

        let loaded = ProjectLoader::new(&config, &root)
            .sources(strict(false))
            .unwrap();
        assert_eq!(
            loaded.experimental_packages,
            BTreeSet::from(["Main".to_string(), "Opted".to_string()])
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unflagged_manifests_leave_experimental_packages_empty() {
        let root = unique_temp("not-experimental");
        scaffold(&root, "greeter");
        let config = manifest::load_project(&root).unwrap().unwrap();

        let loaded = ProjectLoader::new(&config, &root)
            .sources(strict(false))
            .unwrap();
        assert!(loaded.experimental_packages.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn strict_rejects_duplicate_package_name() {
        let root = unique_temp("dup");
        scaffold(&root, "main");
        let config = manifest::load_project(&root).unwrap().unwrap();

        let result = ProjectLoader::new(&config, &root).sources(strict(false));
        assert!(result.is_err());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn lenient_skips_broken_dependency() {
        let root = unique_temp("lenient");
        write(
            &root.join("koja.toml"),
            "[project]\nname = \"main\"\nversion = \"0.1.0\"\n\n[dependencies]\nghost = { path = \"libs/ghost\" }\n",
        );
        write(&root.join("src/main.koja"), "// main\n");
        let config = manifest::load_project(&root).unwrap().unwrap();

        let loaded = ProjectLoader::new(&config, &root)
            .sources(lenient(Dependencies::Sync))
            .unwrap();
        assert_eq!(
            file_names(
                &loaded
                    .sources
                    .iter()
                    .map(|s| s.path.clone())
                    .collect::<Vec<_>>()
            ),
            ["main.koja"]
        );
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("ghost"));
        assert!(loaded.warnings[0].ends_with(", skipping dependencies"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn on_disk_skips_unpinned_git_dep_with_a_warning() {
        let root = unique_temp("on-disk");
        write(
            &root.join("koja.toml"),
            "[project]\nname = \"main\"\nversion = \"0.1.0\"\n\n[dependencies]\ngreeter = { path = \"libs/greeter\" }\nremote = { git = \"https://example.com/remote\" }\n",
        );
        write(&root.join("src/main.koja"), "// main\n");
        write(
            &root.join("libs/greeter/koja.toml"),
            "[project]\nname = \"greeter\"\nversion = \"0.1.0\"\n",
        );
        write(&root.join("libs/greeter/src/greeter.koja"), "// greeter\n");
        let config = manifest::load_project(&root).unwrap().unwrap();

        let loaded = ProjectLoader::new(&config, &root)
            .sources(lenient(Dependencies::OnDisk))
            .unwrap();
        assert_eq!(packages(&loaded.sources), ["Main", "Greeter"]);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.warnings[0].contains("`remote` is not pinned"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn overlays_replace_disk_text_by_canonical_path() {
        let root = unique_temp("overlay");
        scaffold(&root, "greeter");
        let config = manifest::load_project(&root).unwrap().unwrap();

        let main = root.join("src/main.koja").canonicalize().unwrap();
        let overlays = HashMap::from([(main.clone(), "// edited\n".to_string())]);
        let loaded = ProjectLoader::new(&config, &root)
            .overlays(&overlays)
            .sources(strict(false))
            .unwrap();
        let edited = loaded
            .sources
            .iter()
            .find(|s| s.path.ends_with("main.koja"))
            .unwrap();
        assert_eq!(edited.source, "// edited\n");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stdlib_leads_the_bundle_and_skips_claimed_packages() {
        let root = unique_temp("stdlib");
        scaffold(&root, "greeter");
        let config = manifest::load_project(&root).unwrap().unwrap();

        let loaded = ProjectLoader::new(&config, &root)
            .sources(LoadOptions {
                stdlib: Some(StdlibOptions {
                    extraction_root: None,
                    link_tests: true,
                }),
                ..strict(false)
            })
            .unwrap();
        let order = packages(&loaded.sources);
        assert_eq!(order[0], "Global");
        let main = order.iter().position(|p| *p == "Main").unwrap();
        let test = order.iter().position(|p| *p == "Test").unwrap();
        assert!(test < main);
        assert_eq!(&order[main..], ["Main", "Greeter"]);
        assert!(
            loaded
                .sources
                .iter()
                .all(|s| (s.origin == SourceOrigin::Stdlib)
                    == (s.package != "Main" && s.package != "Greeter"))
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn stdlib_rule_links_test_only_on_request() {
        let none = BTreeSet::new();
        let without = stdlib_sources(
            &none,
            &StdlibOptions {
                extraction_root: None,
                link_tests: false,
            },
        );
        assert!(!packages(&without).contains(&"Test"));
        assert!(packages(&without).contains(&"JSON"));

        let with = stdlib_sources(
            &none,
            &StdlibOptions {
                extraction_root: None,
                link_tests: true,
            },
        );
        assert!(packages(&with).contains(&"Test"));
    }

    #[test]
    fn stdlib_rule_for_global_itself_keeps_only_the_test_build() {
        let global = BTreeSet::from(["Global".to_string()]);
        let build = stdlib_sources(
            &global,
            &StdlibOptions {
                extraction_root: None,
                link_tests: false,
            },
        );
        assert!(build.is_empty());

        let test = stdlib_sources(
            &global,
            &StdlibOptions {
                extraction_root: None,
                link_tests: true,
            },
        );
        assert_eq!(packages(&test), ["JSON", "Test"]);
    }

    #[test]
    fn stdlib_rule_roots_paths_at_the_extraction() {
        let none = BTreeSet::new();
        let rooted = stdlib_sources(
            &none,
            &StdlibOptions {
                extraction_root: Some(PathBuf::from("/stdlib")),
                link_tests: false,
            },
        );
        assert!(rooted.iter().all(|s| s.path.starts_with("/stdlib")));
    }
}
