//! `koja deps outdated`: compare every git dependency's pin with what
//! its remote offers now.
//!
//! A `tag` pin is compared against the remote's tag list as versions,
//! so `tag = "2026.3.0"` reports `2026.4.0` when that tag exists. Any
//! other pin (`branch`, `rev`, or the default branch) is compared as a
//! commit against the remote head. The command reads the remote and
//! prints, and never writes `koja.lock`.

use std::path::Path;
use std::process;

use super::git;
use super::lock::Lockfile;
use super::short;
use crate::commands::load_project_or_exit;
use crate::project::{DepSource, GitRef};

/// `koja deps outdated`. Exits 1 when any dependency has something
/// newer, so CI can use it as a check.
pub(crate) fn cmd_outdated(project_root: Option<&Path>) {
    let (config, root) =
        load_project_or_exit(project_root, &["error: `koja deps` requires a koja.toml"]);
    let lockfile = Lockfile::load(&root).unwrap_or_else(|err| {
        eprintln!("error: {err}");
        process::exit(1);
    });

    let mut aliases: Vec<&String> = config.dependencies.keys().collect();
    aliases.sort();

    let mut checked = 0;
    let mut outdated = 0;
    let mut unknown = 0;
    for alias in aliases {
        let (reference, url) = match config.dependencies[alias].source(alias) {
            Ok(DepSource::Git { reference, url }) => (reference, url),
            Ok(DepSource::Path(_)) => continue,
            Err(err) => {
                eprintln!("error: {err}");
                process::exit(1);
            }
        };
        checked += 1;

        let source = format!("git+{url}");
        let locked = lockfile.find(&source, &reference.requirement());
        let name = locked.map_or(alias.as_str(), |entry| entry.name.as_str());

        match check(&url, &reference, locked.map(|entry| entry.rev.as_str())) {
            Ok(Status::Current) => {}
            Ok(Status::Outdated(line)) => {
                outdated += 1;
                println!("  {name} {line}");
            }
            Ok(Status::Unknown(reason)) => {
                unknown += 1;
                println!("  {name} {reason}");
            }
            Err(err) => {
                eprintln!("error: {name}: {err}");
                process::exit(1);
            }
        }
    }

    if checked == 0 {
        println!("no git dependencies");
    } else if outdated == 0 && unknown == 0 {
        println!("all dependencies up to date");
    } else if outdated > 0 {
        process::exit(1);
    }
}

enum Status {
    Current,
    /// The text after the package name, e.g. `2026.3.0 -> 2026.4.0`.
    Outdated(String),
    /// Nothing to compare against, with the reason.
    Unknown(String),
}

fn check(url: &str, reference: &GitRef, locked_rev: Option<&str>) -> Result<Status, String> {
    match reference {
        GitRef::Tag(tag) => {
            let Some(current) = tag_version(tag) else {
                return Ok(Status::Unknown(format!(
                    "tag = {tag} is not an X.Y.Z version, so it cannot be compared"
                )));
            };
            let tags = git::list_tags(url)?;
            Ok(match newest_tag(&tags, current) {
                Some(newest) => Status::Outdated(format!("{tag} -> {newest}")),
                None => Status::Current,
            })
        }
        _ => {
            let Some(locked) = locked_rev else {
                return Ok(Status::Unknown("unlocked, run `koja deps get`".to_string()));
            };
            // A `rev` pin is fixed by definition, so it is measured
            // against the default branch the way a bare dependency is.
            let head_of = match reference {
                GitRef::Rev(_) => GitRef::DefaultBranch,
                other => other.clone(),
            };
            let head = git::resolve_ref(url, &head_of)?;
            Ok(if head == locked {
                Status::Current
            } else {
                Status::Outdated(format!(
                    "{} -> {} ({})",
                    short(locked),
                    short(&head),
                    reference.requirement()
                ))
            })
        }
    }
}

/// `X.Y.Z` with an optional leading `v`, as a comparable triple.
fn tag_version(tag: &str) -> Option<(u64, u64, u64)> {
    let mut numbers = tag.strip_prefix('v').unwrap_or(tag).split('.');
    let major = numbers.next()?.parse().ok()?;
    let minor = numbers.next()?.parse().ok()?;
    let patch = numbers.next()?.parse().ok()?;
    if numbers.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// The highest version tag above `current`, or `None` when the pin
/// is the newest. Tags that are not versions are ignored.
fn newest_tag(tags: &[String], current: (u64, u64, u64)) -> Option<&str> {
    tags.iter()
        .filter_map(|tag| tag_version(tag).map(|version| (version, tag.as_str())))
        .filter(|(version, _)| *version > current)
        .max_by_key(|(version, _)| *version)
        .map(|(_, tag)| tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn tag_version_reads_three_numbers_with_optional_v() {
        assert_eq!(tag_version("2026.3.0"), Some((2026, 3, 0)));
        assert_eq!(tag_version("v0.19.0"), Some((0, 19, 0)));
        assert_eq!(tag_version("1.2"), None);
        assert_eq!(tag_version("1.2.3.4"), None);
        assert_eq!(tag_version("release-1"), None);
        assert_eq!(tag_version("v1.x.0"), None);
    }

    #[test]
    fn newest_tag_picks_the_highest_version_above_the_pin() {
        let all = tags(&["2026.2.0", "2026.3.0", "2026.4.0", "2026.3.1", "nightly"]);
        assert_eq!(newest_tag(&all, (2026, 3, 0)), Some("2026.4.0"));
        assert_eq!(newest_tag(&all, (2026, 4, 0)), None);
        assert_eq!(newest_tag(&all, (2027, 0, 0)), None);
    }

    #[test]
    fn newest_tag_orders_numerically_not_lexically() {
        let all = tags(&["v0.9.0", "v0.10.0", "v0.2.0"]);
        assert_eq!(newest_tag(&all, (0, 9, 0)), Some("v0.10.0"));
    }
}
