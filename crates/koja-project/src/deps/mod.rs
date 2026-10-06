//! Dependency resolution. Compiling commands run the offline sync,
//! `koja deps get` runs the fetch, and editors run the on-disk read.
//!
//! [`fetch_project`] is the only path that produces a lock to write.
//! Compiles run [`sync_project`], which verifies the lock against
//! the manifest and re-materializes `deps/` from the mirror cache,
//! erroring with an actionable message instead of fetching. Editors
//! run [`resolve_on_disk`], which reads what is there and reports
//! what is not as warnings, so a half-set-up project still gets
//! diagnostics for the files it has.
//!
//! `deps/<Package>/` trees are read-only copies of an exact commit,
//! stamped with a `.koja-rev` marker. A marker mismatch triggers a
//! re-export into a temp dir that is renamed into place, so a
//! concurrent build never sees a half-copied dep.

pub mod git;
pub mod lock;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::{fs, process};

use crate::manifest::{self, DepSource, GitRef, ProjectConfig};
use lock::{LockedPackage, Lockfile};

const REV_MARKER: &str = ".koja-rev";

/// One package in the resolved dependency graph, ready for the
/// loader to walk.
pub struct ResolvedDep {
    /// The dep's own `experimental = true` opt-in, which silences
    /// experimental warnings inside its files only.
    pub experimental: bool,
    /// Lowercase package identity (the `deps/<name>` directory).
    pub name: String,
    /// PascalCase code namespace stamped on the package's files.
    pub namespace: String,
    pub root: PathBuf,
    pub src: Vec<String>,
    /// The dep's own manifest task exports (`[tasks]`), kept for task
    /// resolution across the dependency graph.
    pub tasks: HashMap<String, String>,
}

/// Which pins `koja deps get`/`update` re-resolve against the remote.
pub enum UpdateSpec {
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
    /// Lock and cache only, where any miss is an error naming the fix.
    Offline,
    /// `deps/` as it is, where a git dep that is not pinned or not
    /// materialized is skipped with a warning. Never fetches, never
    /// materializes, never writes.
    OnDisk,
}

/// The outcome of a fetch, which is the graph for the loader and the
/// lock to write.
pub struct Fetched {
    pub lock: Lockfile,
    pub resolved: Vec<ResolvedDep>,
}

/// The outcome of an on-disk read, which is the graph of what is
/// present and one warning per git dependency that was skipped.
pub struct OnDisk {
    pub resolved: Vec<ResolvedDep>,
    pub warnings: Vec<String>,
}

/// Offline sync: verify the lock, materialize stale deps from the
/// cache, and return the transitive graph for the loader. Never
/// touches the network and never writes the lock.
pub fn sync_project(config: &ProjectConfig, root: &Path) -> Result<Vec<ResolvedDep>, String> {
    let resolver = resolve(config, root, Mode::Offline)?;
    Ok(resolver.resolved)
}

/// On-disk read for editors. Path deps resolve in place, and a git
/// dep joins the graph only when the lock pins it and `deps/<name>`
/// holds that commit. Each skipped git dep is named in `warnings`.
/// Manifest and graph errors (a missing path, a cycle, a duplicate
/// name) are still `Err`, since no file set is right in their
/// presence.
pub fn resolve_on_disk(config: &ProjectConfig, root: &Path) -> Result<OnDisk, String> {
    let resolver = resolve(config, root, Mode::OnDisk)?;
    Ok(OnDisk {
        resolved: resolver.resolved,
        warnings: resolver.warnings,
    })
}

/// Fetch for `koja deps get`. Resolve every git ref against its
/// remote, honoring pins that `update` leaves alone, and materialize
/// into `deps/`. The caller writes the returned lock.
pub fn fetch_project(
    config: &ProjectConfig,
    root: &Path,
    update: UpdateSpec,
) -> Result<Fetched, String> {
    let resolver = resolve(config, root, Mode::Fetch(update))?;
    Ok(Fetched {
        lock: Lockfile {
            packages: resolver.locked,
        },
        resolved: resolver.resolved,
    })
}

/// The local state of one locked git dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepState {
    /// `deps/<name>` holds the pinned commit.
    Materialized,
    /// The cache has the commit but `deps/<name>` is missing or on
    /// another commit. The next build re-materializes it.
    Stale,
    /// Neither `deps/<name>` nor the cache holds the commit.
    NotFetched,
}

pub fn dep_state(root: &Path, entry: &LockedPackage) -> DepState {
    let dep_root = root.join("deps").join(&entry.name);
    if read_marker(&dep_root).as_deref() == Some(entry.rev.as_str()) {
        return DepState::Materialized;
    }
    let url = entry.source.trim_start_matches("git+");
    match git::mirror_dir(url) {
        Ok(mirror) if mirror.is_dir() && git::has_commit(&mirror, &entry.rev) => DepState::Stale,
        _ => DepState::NotFetched,
    }
}

fn resolve(config: &ProjectConfig, root: &Path, mode: Mode) -> Result<Resolver, String> {
    let mut seen_names: BTreeMap<String, String> = stdlib_reserved_names()
        .into_iter()
        .map(|name| (name, "the standard library".to_string()))
        .collect();
    seen_names.insert(config.name.clone(), "the project".to_string());
    seen_names.insert(config.namespace(), "the project".to_string());

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
        warnings: Vec::new(),
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
    /// Git deps skipped in [`Mode::OnDisk`], one line each.
    warnings: Vec<String>,
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
        self.claim_package(&dep_config, declared_by)?;
        self.resolved.push(ResolvedDep {
            experimental: dep_config.experimental,
            name: dep_config.name.clone(),
            namespace: dep_config.namespace(),
            root: canonical.clone(),
            src: dep_config.src.clone(),
            tasks: dep_config.tasks.clone(),
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
            Mode::OnDisk => match self.on_disk_git_dep(alias, &source, &requirement) {
                Ok(found) => found,
                Err(warning) => {
                    self.warnings.push(warning);
                    return Ok(());
                }
            },
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

        let dep_config = load_dep_manifest(&dep_root, alias)?;
        self.claim_package(&dep_config, declared_by)?;
        if matches!(self.mode, Mode::Fetch(_)) {
            self.locked.push(LockedPackage {
                name: name.clone(),
                requirement,
                rev,
                source,
            });
        }

        self.resolved.push(ResolvedDep {
            experimental: dep_config.experimental,
            name,
            namespace: dep_config.namespace(),
            root: dep_root.clone(),
            src: dep_config.src.clone(),
            tasks: dep_config.tasks.clone(),
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

    /// On disk, the lock must pin this dep and `deps/<name>` must
    /// already hold that commit. Anything else is the warning the
    /// caller shows, never a fetch or a materialization.
    fn on_disk_git_dep(
        &self,
        alias: &str,
        source: &str,
        requirement: &str,
    ) -> Result<(String, PathBuf, String), String> {
        let entry = self.lock.find(source, requirement).ok_or_else(|| {
            format!("dependency `{alias}` is not pinned in koja.lock, run `koja deps get`")
        })?;
        let dep_root = self.deps_dir.join(&entry.name);
        if read_marker(&dep_root).as_deref() != Some(entry.rev.as_str()) {
            return Err(format!(
                "dependency `{}` is not in deps/, run `koja deps get`",
                entry.name
            ));
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

        let config = manifest::load_project(&temp)
            .map_err(|err| format!("dependency at `{url}`: {err}"))?
            .ok_or_else(|| format!("`{url}` is not a koja package (no koja.toml at its root)"))?;
        let name = config.name;

        write_marker(&temp, rev)?;
        let dep_root = self.deps_dir.join(&name);
        swap_into_place(&temp, &dep_root)?;
        Ok((name, dep_root))
    }

    /// Claim a dep's identity strings. `name` and `namespace` live in
    /// case-disjoint worlds (lowercase vs PascalCase), so one map holds
    /// both without cross-talk, and two deps collide when either matches.
    fn claim_package(&mut self, config: &ProjectConfig, declared_by: &str) -> Result<(), String> {
        self.claim(&config.name, "name", declared_by)?;
        self.claim(&config.namespace(), "namespace", declared_by)
    }

    fn claim(&mut self, claimed: &str, kind: &str, declared_by: &str) -> Result<(), String> {
        if let Some(existing) = self
            .seen_names
            .insert(claimed.to_string(), declared_by.to_string())
        {
            return Err(format!(
                "duplicate package {kind} `{claimed}` in dependency graph (declared by {existing} and {declared_by})"
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
    manifest::load_project(dep_root)
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
pub fn remove_tree(path: &Path) -> Result<(), String> {
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

/// Identity strings reserved by the embedded stdlib, in both worlds. A
/// dependency claiming a stdlib namespace (`JSON`) or the matching
/// lowercase name (`json`) would collide with the bundled sources.
fn stdlib_reserved_names() -> BTreeSet<String> {
    koja_stdlib::qualified_sources()
        .into_iter()
        .map(|source| source.package)
        .chain(std::iter::once("Global".to_string()))
        .flat_map(|namespace| {
            let name = namespace.to_lowercase();
            [namespace, name]
        })
        .collect()
}

/// The first seven characters of a commit SHA, the form the CLI
/// prints.
pub fn short(rev: &str) -> &str {
    &rev[..rev.len().min(7)]
}
