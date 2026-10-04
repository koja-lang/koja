//! The `koja deps` command family.
//!
//! `koja deps get` and `koja deps update` are the only paths that
//! write `koja.lock`. `koja deps outdated` reads the remotes and
//! prints. The resolver itself lives in [`koja_project::deps`], and
//! every compiling command runs its offline sync through the loader.

mod outdated;

pub(crate) use outdated::cmd_outdated;

use std::collections::BTreeSet;
use std::path::Path;
use std::{fs, process};

use koja_project::DepSource;
use koja_project::deps::lock::Lockfile;
use koja_project::deps::{DepState, UpdateSpec, dep_state, fetch_project, git, remove_tree, short};

use crate::commands::load_project_or_exit;

/// `koja deps get` / `koja deps update [name]`.
pub(crate) fn cmd_get(project_root: Option<&Path>, update: Option<Option<String>>) {
    let (config, root) =
        load_project_or_exit(project_root, &["error: `koja deps` requires a koja.toml"]);
    let spec = match update {
        None => UpdateSpec::None,
        Some(None) => UpdateSpec::All,
        Some(Some(name)) => UpdateSpec::Only(name),
    };

    let fetched = fetch_project(&config, &root, spec).unwrap_or_else(|err| {
        eprintln!("error: {err}");
        process::exit(1);
    });

    let lockfile = fetched.lock;
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
pub(crate) fn cmd_status(project_root: Option<&Path>) {
    let (config, root) =
        load_project_or_exit(project_root, &["error: `koja deps` requires a koja.toml"]);
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
                        describe(dep_state(&root, entry))
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

fn describe(state: DepState) -> &'static str {
    match state {
        DepState::Materialized => "ok",
        DepState::Stale => "stale deps/, re-materializes on next build",
        DepState::NotFetched => "not fetched, run `koja deps get`",
    }
}

/// `koja deps clean [--cache]`: remove the materialized `deps/`
/// tree (read-only, so plain `rm -rf` chokes on it), and optionally
/// the global mirror cache. Never touches koja.lock.
pub(crate) fn cmd_clean(project_root: Option<&Path>, cache: bool) {
    let (_, root) =
        load_project_or_exit(project_root, &["error: `koja deps` requires a koja.toml"]);
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
