//! Dependency resolution: the `koja deps` command family plus the
//! offline sync every compiling command runs.
//!
//! `koja deps get` and `koja deps update` are the only paths that
//! touch the network or write `koja.lock`. Everything else runs
//! [`sync_project`], which verifies the lock against the manifest and
//! re-materializes `deps/` from the mirror cache, erroring with an
//! actionable message instead of fetching.
//!
//! `deps/<Package>/` trees are read-only copies of an exact commit,
//! stamped with a `.koja-rev` marker. A marker mismatch triggers a
//! re-export into a temp dir that is renamed into place, so a
//! concurrent build never sees a half-copied dep.

mod git;
mod lock;

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::{fs, process};

use crate::commands::load_project_or_exit;
use crate::project::{self, DepSource, GitRef, ProjectConfig};
use lock::{LockedPackage, Lockfile};

const REV_MARKER: &str = ".koja-rev";

/// One package in the resolved dependency graph, ready for the
/// loader to walk.
pub(crate) struct ResolvedDep {
    pub name: String,
    pub root: PathBuf,
    pub src: Vec<String>,
}

/// Which pins `koja deps get`/`update` re-resolve against the remote.
enum UpdateSpec {
    All,
    None,
    Only(String),
}

impl UpdateSpec {
    fn matches(&self, name: &str) -> bool {
        match self {
            UpdateSpec::All => true,
            UpdateSpec::None => false,
            UpdateSpec::Only(only) => only == name,
        }
    }
}

enum Mode {
    /// Resolve refs and fetch over the network (`koja deps get`).
    Fetch(UpdateSpec),
    /// Lock and cache only; any miss is an error naming the fix.
    Offline,
}

/// Offline sync: verify the lock, materialize stale deps from the
/// cache, and return the transitive graph for the loader. Never
/// touches the network and never writes the lock.
pub(crate) fn sync_project(
    config: &ProjectConfig,
    root: &Path,
) -> Result<Vec<ResolvedDep>, String> {
    let resolver = resolve(config, root, Mode::Offline)?;
    Ok(resolver.resolved)
}

/// `koja deps get` / `koja deps update [name]`.
pub(crate) fn cmd_get(update: Option<Option<String>>) {
    let (config, root) = load_project_or_exit(&["error: `koja deps` requires a koja.toml"]);
    let spec = match update {
        None => UpdateSpec::None,
        Some(None) => UpdateSpec::All,
        Some(Some(name)) => UpdateSpec::Only(name),
    };

    let resolver = resolve(&config, &root, Mode::Fetch(spec)).unwrap_or_else(|err| {
        eprintln!("error: {err}");
        process::exit(1);
    });

    let lockfile = Lockfile {
        packages: resolver.locked,
    };
    let changed = lockfile.write_if_changed(&root).unwrap_or_else(|err| {
        eprintln!("error: {err}");
        process::exit(1);
    });

    for package in &lockfile.packages {
        println!(
            "  {} {} ({})",
            package.name,
            short(&package.rev),
            package.requirement
        );
    }
    if lockfile.packages.is_empty() {
        println!("no git dependencies");
    } else if changed {
        println!("koja.lock updated");
    } else {
        println!("koja.lock up to date");
    }
}

/// Bare `koja deps`: print each dependency with its pin and local
/// state. Offline and side-effect free.
pub(crate) fn cmd_status() {
    let (config, root) = load_project_or_exit(&["error: `koja deps` requires a koja.toml"]);
    let lockfile = Lockfile::load(&root).unwrap_or_else(|err| {
        eprintln!("error: {err}");
        process::exit(1);
    });

    if config.dependencies.is_empty() && lockfile.packages.is_empty() {
        println!("no dependencies");
        return;
    }

    let mut declared_sources = BTreeSet::new();
    let mut aliases: Vec<&String> = config.dependencies.keys().collect();
    aliases.sort();
    for alias in aliases {
        let dep = &config.dependencies[alias];
        match dep.source(alias) {
            Ok(DepSource::Path(path)) => println!("  {alias} path = {path}"),
            Ok(DepSource::Git { reference, url }) => {
                let source = format!("git+{url}");
                let requirement = reference.requirement();
                match lockfile.find(&source, &requirement) {
                    Some(entry) => println!(
                        "  {} {} ({requirement}) {}",
                        entry.name,
                        short(&entry.rev),
                        git_dep_state(&root, entry)
                    ),
                    None => println!("  {alias} ({requirement}) unlocked, run `koja deps get`"),
                }
                declared_sources.insert(source);
            }
            Err(err) => println!("  {alias} invalid: {err}"),
        }
    }

    for entry in &lockfile.packages {
        if !declared_sources.contains(&entry.source) {
            println!(
                "  {} {} (transitive or unused)",
                entry.name,
                short(&entry.rev)
            );
        }
    }
}

/// `koja deps clean [--cache]`: remove the materialized `deps/`
/// tree (read-only, so plain `rm -rf` chokes on it), and optionally
/// the global mirror cache. Never touches koja.lock.
pub(crate) fn cmd_clean(cache: bool) {
    let (_, root) = load_project_or_exit(&["error: `koja deps` requires a koja.toml"]);
    let deps_dir = root.join("deps");
    if deps_dir.exists() {
        if let Err(err) = remove_tree(&deps_dir) {
            eprintln!("error: {err}");
            process::exit(1);
        }
        println!("removed {}", deps_dir.display());
    }
    if cache {
        match git::cache_dir() {
            Ok(dir) if dir.exists() => {
                if let Err(err) = fs::remove_dir_all(&dir) {
                    eprintln!("error: cannot remove {}: {err}", dir.display());
                    process::exit(1);
                }
                println!("removed {}", dir.display());
            }
            Ok(_) => {}
            Err(err) => {
                eprintln!("error: {err}");
                process::exit(1);
            }
        }
    }
}

fn resolve(config: &ProjectConfig, root: &Path, mode: Mode) -> Result<Resolver, String> {
    let mut seen_names: BTreeMap<String, String> = stdlib_package_names()
        .into_iter()
        .map(|name| (name, "the standard library".to_string()))
        .collect();
    seen_names.insert(config.name.clone(), "the project".to_string());

    let mut resolver = Resolver {
        deps_dir: root.join("deps"),
        lock: Lockfile::load(root)?,
        locked: Vec::new(),
        mode,
        resolved: Vec::new(),
        seen_names,
        seen_paths: BTreeSet::new(),
        seen_sources: BTreeMap::new(),
        stack: Vec::new(),
    };
    resolver.walk(config, root, None)?;
    Ok(resolver)
}

/// Requirement + declarer recorded per canonical URL, for diamond
/// dedup and the conflicting-requirements error.
struct SourceSeen {
    declared_by: String,
    requirement: String,
}

struct Resolver {
    deps_dir: PathBuf,
    /// Prior lock (pins). Never written by the resolver.
    lock: Lockfile,
    /// Lock entries for the new lockfile, built in fetch mode.
    locked: Vec<LockedPackage>,
    mode: Mode,
    resolved: Vec<ResolvedDep>,
    /// Package name -> declarer, seeded with the project and stdlib.
    seen_names: BTreeMap<String, String>,
    seen_paths: BTreeSet<PathBuf>,
    seen_sources: BTreeMap<String, SourceSeen>,
    /// DFS stack of urls / canonical paths, for cycle reporting.
    stack: Vec<String>,
}

impl Resolver {
    /// Walk one manifest's `[dependencies]` in alias order. `jail`,
    /// when set, is the git checkout that path deps must stay inside.
    fn walk(
        &mut self,
        config: &ProjectConfig,
        base: &Path,
        jail: Option<&Path>,
    ) -> Result<(), String> {
        let mut aliases: Vec<&String> = config.dependencies.keys().collect();
        aliases.sort();
        for alias in aliases {
            let declared_by = format!("`{}`", config.name);
            match config.dependencies[alias].source(alias)? {
                DepSource::Path(path) => {
                    self.walk_path_dep(alias, &path, base, jail, &declared_by)?;
                }
                DepSource::Git { reference, url } => {
                    self.walk_git_dep(alias, &url, &reference, &declared_by)?;
                }
            }
        }
        Ok(())
    }

    fn walk_path_dep(
        &mut self,
        alias: &str,
        path: &str,
        base: &Path,
        jail: Option<&Path>,
        declared_by: &str,
    ) -> Result<(), String> {
        let dir = base.join(path);
        let canonical = dir.canonicalize().map_err(|_| {
            format!(
                "dependency `{alias}`: path `{}` does not exist",
                dir.display()
            )
        })?;
        if let Some(jail) = jail
            && !canonical.starts_with(jail)
        {
            return Err(format!(
                "dependency `{alias}`: path `{path}` escapes its git dependency checkout"
            ));
        }

        let key = canonical.to_string_lossy().into_owned();
        if self.stack.contains(&key) {
            return Err(self.cycle_error(&key));
        }
        if !self.seen_paths.insert(canonical.clone()) {
            return Ok(());
        }

        let dep_config = load_dep_manifest(&canonical, alias)?;
        self.claim_name(&dep_config.name, declared_by)?;
        self.resolved.push(ResolvedDep {
            name: dep_config.name.clone(),
            root: canonical.clone(),
            src: dep_config.src.clone(),
        });

        self.stack.push(key);
        self.walk(&dep_config, &canonical, jail)?;
        self.stack.pop();
        Ok(())
    }

    fn walk_git_dep(
        &mut self,
        alias: &str,
        url: &str,
        reference: &GitRef,
        declared_by: &str,
    ) -> Result<(), String> {
        let requirement = reference.requirement();
        if let Some(seen) = self.seen_sources.get(url) {
            if seen.requirement != requirement {
                return Err(format!(
                    "conflicting requirements for `{url}`: {declared_by} wants `{requirement}`, {} wants `{}`",
                    seen.declared_by, seen.requirement
                ));
            }
            if self.stack.contains(&url.to_string()) {
                return Err(self.cycle_error(url));
            }
            return Ok(());
        }
        self.seen_sources.insert(
            url.to_string(),
            SourceSeen {
                declared_by: declared_by.to_string(),
                requirement: requirement.clone(),
            },
        );

        let source = format!("git+{url}");
        let (name, dep_root, rev) = match &self.mode {
            Mode::Offline => self.sync_git_dep(alias, url, &source, &requirement)?,
            Mode::Fetch(update) => {
                let pinned = self
                    .lock
                    .find(&source, &requirement)
                    .filter(|entry| !update.matches(&entry.name))
                    .cloned();
                self.fetch_git_dep(url, reference, pinned)?
            }
        };
        // Canonical so it can serve as the jail for the dep's own
        // path deps (`starts_with` fails on symlinked prefixes like
        // macOS's /tmp otherwise).
        let dep_root = dep_root
            .canonicalize()
            .map_err(|err| format!("cannot resolve {}: {err}", dep_root.display()))?;

        self.claim_name(&name, declared_by)?;
        if matches!(self.mode, Mode::Fetch(_)) {
            self.locked.push(LockedPackage {
                name: name.clone(),
                requirement,
                rev,
                source,
            });
        }

        let dep_config = load_dep_manifest(&dep_root, alias)?;
        self.resolved.push(ResolvedDep {
            name,
            root: dep_root.clone(),
            src: dep_config.src.clone(),
        });

        self.stack.push(url.to_string());
        self.walk(&dep_config, &dep_root, Some(&dep_root))?;
        self.stack.pop();
        Ok(())
    }

    /// Offline: the lock must pin this dep and the cache must already
    /// hold the commit.
    fn sync_git_dep(
        &self,
        alias: &str,
        url: &str,
        source: &str,
        requirement: &str,
    ) -> Result<(String, PathBuf, String), String> {
        let entry = self.lock.find(source, requirement).ok_or_else(|| {
            format!(
                "dependency `{alias}` is not pinned in koja.lock (koja.toml changed?), run `koja deps get`"
            )
        })?;
        let dep_root = self.deps_dir.join(&entry.name);
        if read_marker(&dep_root).as_deref() != Some(entry.rev.as_str()) {
            let mirror = git::mirror_dir(url)?;
            if !mirror.is_dir() || !git::has_commit(&mirror, &entry.rev) {
                return Err(format!(
                    "dependency `{}` is not fetched, run `koja deps get`",
                    entry.name
                ));
            }
            materialize(&mirror, &entry.rev, &dep_root)?;
        }
        Ok((entry.name.clone(), dep_root, entry.rev.clone()))
    }

    /// Fetch mode: honor an existing pin, otherwise resolve the ref
    /// against the remote. Either way the commit lands in the cache
    /// and materializes into `deps/`.
    fn fetch_git_dep(
        &self,
        url: &str,
        reference: &GitRef,
        pinned: Option<LockedPackage>,
    ) -> Result<(String, PathBuf, String), String> {
        let rev = match &pinned {
            Some(entry) => entry.rev.clone(),
            None => git::resolve_ref(url, reference)?,
        };

        let mirror = git::ensure_mirror(url)?;
        if !git::has_commit(&mirror, &rev) {
            git::fetch(&mirror, url)?;
        }
        if !git::has_commit(&mirror, &rev) {
            return Err(format!(
                "pinned commit {} for `{url}` is no longer reachable on the remote \
                 (force-pushed away?), run `koja deps update` to re-resolve",
                short(&rev)
            ));
        }

        match pinned {
            Some(entry) => {
                let dep_root = self.deps_dir.join(&entry.name);
                materialize(&mirror, &rev, &dep_root)?;
                Ok((entry.name, dep_root, rev))
            }
            None => {
                let (name, dep_root) = self.materialize_fresh(url, &mirror, &rev)?;
                Ok((name, dep_root, rev))
            }
        }
    }

    /// First materialization of a dep whose package name is unknown
    /// until its koja.toml is read: export to a temp dir, read the
    /// name, then swap into `deps/<Name>`.
    fn materialize_fresh(
        &self,
        url: &str,
        mirror: &Path,
        rev: &str,
    ) -> Result<(String, PathBuf), String> {
        let temp = self.deps_dir.join(format!(".koja-tmp-{}", process::id()));
        remove_tree(&temp)?;
        git::export_tree(mirror, rev, &temp)?;

        let config = project::load_project(&temp)
            .map_err(|err| format!("dependency at `{url}`: {err}"))?
            .ok_or_else(|| format!("`{url}` is not a koja package (no koja.toml at its root)"))?;
        let name = config.name;

        write_marker(&temp, rev)?;
        let dep_root = self.deps_dir.join(&name);
        swap_into_place(&temp, &dep_root)?;
        Ok((name, dep_root))
    }

    fn claim_name(&mut self, name: &str, declared_by: &str) -> Result<(), String> {
        if let Some(existing) = self
            .seen_names
            .insert(name.to_string(), declared_by.to_string())
        {
            return Err(format!(
                "duplicate package name `{name}` in dependency graph (declared by {existing} and {declared_by})"
            ));
        }
        Ok(())
    }

    fn cycle_error(&self, repeated: &str) -> String {
        let mut chain: Vec<&str> = self.stack.iter().map(String::as_str).collect();
        chain.push(repeated);
        format!("dependency cycle: {}", chain.join(" -> "))
    }
}

/// Export the tree at `rev` into `dest` unless its marker already
/// matches. Builds in a temp sibling and renames into place.
fn materialize(mirror: &Path, rev: &str, dest: &Path) -> Result<(), String> {
    if read_marker(dest).as_deref() == Some(rev) {
        return Ok(());
    }
    let parent = dest.parent().expect("deps dir has a parent");
    let temp = parent.join(format!(".koja-tmp-{}", process::id()));
    remove_tree(&temp)?;
    git::export_tree(mirror, rev, &temp)?;
    write_marker(&temp, rev)?;
    swap_into_place(&temp, dest)
}

/// Move a finished export into place and seal it read-only. Rename must
/// come first because some macOS versions refuse to rename a directory
/// that has no write bit.
fn swap_into_place(temp: &Path, dest: &Path) -> Result<(), String> {
    remove_tree(dest)?;
    fs::rename(temp, dest)
        .map_err(|err| format!("cannot move dependency into {}: {err}", dest.display()))?;
    set_tree_writable(dest, false)
}

fn read_marker(dep_root: &Path) -> Option<String> {
    fs::read_to_string(dep_root.join(REV_MARKER))
        .ok()
        .map(|contents| contents.trim().to_string())
}

fn write_marker(dep_root: &Path, rev: &str) -> Result<(), String> {
    fs::write(dep_root.join(REV_MARKER), format!("{rev}\n"))
        .map_err(|err| format!("cannot write {}: {err}", REV_MARKER))
}

fn load_dep_manifest(dep_root: &Path, alias: &str) -> Result<ProjectConfig, String> {
    project::load_project(dep_root)
        .map_err(|err| format!("dependency `{alias}`: {err}"))?
        .ok_or_else(|| {
            format!(
                "dependency `{alias}`: no koja.toml found at {}",
                dep_root.display()
            )
        })
}

/// Remove a tree that may be read-only. Files can't be unlinked from
/// unwritable directories, so flip the tree writable first.
fn remove_tree(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    set_tree_writable(path, true)?;
    fs::remove_dir_all(path).map_err(|err| format!("cannot remove {}: {err}", path.display()))
}

/// Recursively add or remove write permission bits.
fn set_tree_writable(path: &Path, writable: bool) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|err| format!("cannot stat {}: {err}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    let mode = metadata.permissions().mode();
    let new_mode = if writable {
        mode | 0o200
    } else {
        mode & !0o222
    };

    // Directories go writable before their entries (so we can descend
    // and rename) and read-only after (so entries flip first).
    if metadata.is_dir() && writable {
        set_mode(path, new_mode)?;
    }
    if metadata.is_dir() {
        let entries =
            fs::read_dir(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
        for entry in entries.flatten() {
            set_tree_writable(&entry.path(), writable)?;
        }
    }
    if !metadata.is_dir() || !writable {
        set_mode(path, new_mode)?;
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|err| format!("cannot chmod {}: {err}", path.display()))
}

/// Package names reserved by the embedded stdlib. A dependency
/// claiming one of these would collide with the bundled sources.
fn stdlib_package_names() -> BTreeSet<String> {
    koja_stdlib::qualified_sources()
        .into_iter()
        .map(|source| source.package)
        .chain(std::iter::once("Global".to_string()))
        .collect()
}

fn short(rev: &str) -> &str {
    &rev[..rev.len().min(7)]
}

fn git_dep_state(root: &Path, entry: &LockedPackage) -> &'static str {
    let dep_root = root.join("deps").join(&entry.name);
    if read_marker(&dep_root).as_deref() == Some(entry.rev.as_str()) {
        return "ok";
    }
    let url = entry.source.trim_start_matches("git+");
    match git::mirror_dir(url) {
        Ok(mirror) if mirror.is_dir() && git::has_commit(&mirror, &entry.rev) => {
            "stale deps/, re-materializes on next build"
        }
        _ => "not fetched, run `koja deps get`",
    }
}
