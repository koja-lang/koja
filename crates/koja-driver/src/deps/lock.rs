//! `koja.lock` reading and writing.
//!
//! The lockfile pins every git dependency (path deps are omitted) to
//! an exact commit SHA. Serialization is hand-rolled and byte
//! deterministic: entries sort by name, attributes are alphabetical,
//! so the output is a pure function of the resolved set.

use std::fs;
use std::path::Path;

use serde::Deserialize;

pub(crate) const LOCK_FILE: &str = "koja.lock";

/// One pinned git dependency.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct LockedPackage {
    pub name: String,
    /// Canonical form of the manifest's ref selector (e.g.
    /// `tag = v0.1.0`). A mismatch against the manifest means the
    /// lock entry is stale.
    pub requirement: String,
    /// Full commit SHA.
    pub rev: String,
    /// `git+<url>`.
    pub source: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Lockfile {
    pub packages: Vec<LockedPackage>,
}

#[derive(Deserialize)]
struct LockToml {
    #[serde(default)]
    package: Vec<LockedPackage>,
    version: u64,
}

impl Lockfile {
    /// Read `koja.lock` from the project root. A missing file is an
    /// empty lock.
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join(LOCK_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let source =
            fs::read_to_string(&path).map_err(|err| format!("error reading koja.lock: {err}"))?;
        let parsed: LockToml =
            toml::from_str(&source).map_err(|err| format!("koja.lock parse error: {err}"))?;
        if parsed.version != 1 {
            return Err(format!(
                "koja.lock version {} is not supported by this compiler",
                parsed.version
            ));
        }
        Ok(Self {
            packages: parsed.package,
        })
    }

    /// The pinned entry for a source + requirement pair. Matching is
    /// by canonical URL, never by package name, since a dep's name
    /// isn't knowable until it has been fetched.
    pub fn find(&self, source: &str, requirement: &str) -> Option<&LockedPackage> {
        self.packages
            .iter()
            .find(|package| package.source == source && package.requirement == requirement)
    }

    pub fn render(&self) -> String {
        let mut packages: Vec<&LockedPackage> = self.packages.iter().collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));

        let mut out = String::from("version = 1\n");
        for package in packages {
            out.push_str(&format!(
                "\n[[package]]\nname = \"{}\"\nrequirement = \"{}\"\nrev = \"{}\"\nsource = \"{}\"\n",
                package.name, package.requirement, package.rev, package.source
            ));
        }
        out
    }

    /// Write the lock to the project root when its rendering differs
    /// from what's on disk. Returns whether a write happened. An
    /// empty lock is only written when a lockfile already exists
    /// (pruning), so path-only projects never grow one.
    pub fn write_if_changed(&self, root: &Path) -> Result<bool, String> {
        let path = root.join(LOCK_FILE);
        let rendered = self.render();
        if self.packages.is_empty() && !path.exists() {
            return Ok(false);
        }
        if fs::read_to_string(&path).is_ok_and(|existing| existing == rendered) {
            return Ok(false);
        }
        fs::write(&path, rendered).map_err(|err| format!("error writing koja.lock: {err}"))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Lockfile {
        Lockfile {
            packages: vec![
                LockedPackage {
                    name: "Zeta".to_string(),
                    requirement: "branch = main".to_string(),
                    rev: "b".repeat(40),
                    source: "git+https://example.com/zeta".to_string(),
                },
                LockedPackage {
                    name: "Alpha".to_string(),
                    requirement: "tag = v1.0".to_string(),
                    rev: "a".repeat(40),
                    source: "git+https://example.com/alpha".to_string(),
                },
            ],
        }
    }

    #[test]
    fn render_sorts_entries_by_name_with_alpha_attributes() {
        let rendered = sample().render();
        let alpha = rendered.find("Alpha").unwrap();
        let zeta = rendered.find("Zeta").unwrap();
        assert!(alpha < zeta);
        assert!(rendered.starts_with("version = 1\n"));

        let expected_entry = format!(
            "[[package]]\nname = \"Alpha\"\nrequirement = \"tag = v1.0\"\nrev = \"{}\"\nsource = \"git+https://example.com/alpha\"\n",
            "a".repeat(40)
        );
        assert!(rendered.contains(&expected_entry));
    }

    #[test]
    fn parse_rewrite_round_trips_byte_equal() {
        let rendered = sample().render();
        let dir = std::env::temp_dir().join(format!("koja-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LOCK_FILE), &rendered).unwrap();

        let reloaded = Lockfile::load(&dir).unwrap();
        assert_eq!(reloaded.render(), rendered);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_matches_on_source_and_requirement() {
        let lock = sample();
        assert!(
            lock.find("git+https://example.com/alpha", "tag = v1.0")
                .is_some()
        );
        assert!(
            lock.find("git+https://example.com/alpha", "tag = v1.1")
                .is_none()
        );
        assert!(
            lock.find("git+https://example.com/other", "tag = v1.0")
                .is_none()
        );
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let dir = std::env::temp_dir().join(format!("koja-lockv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LOCK_FILE), "version = 2\n").unwrap();

        assert!(Lockfile::load(&dir).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}
