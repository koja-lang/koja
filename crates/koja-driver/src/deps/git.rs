//! Shell-outs to the `git` binary: the mirror cache, remote ref
//! resolution, and tree export.
//!
//! Auth is deliberately delegated to ambient git/ssh configuration
//! (credential helpers, `insteadOf` rewrites, SSH agents), which is
//! why this shells out instead of linking libgit2. Subprocesses
//! inherit stdin and stderr so credential and passphrase prompts
//! reach the terminal.

use std::env;
use std::fs::{self, File};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::project::GitRef;

/// Root of the per-user Koja directory: `KOJA_HOME` or `~/.koja`.
pub(crate) fn koja_home() -> Result<PathBuf, String> {
    if let Ok(home) = env::var("KOJA_HOME") {
        return Ok(PathBuf::from(home));
    }
    match env::var("HOME") {
        Ok(home) => Ok(PathBuf::from(home).join(".koja")),
        Err(_) => Err("cannot determine home directory (set KOJA_HOME or HOME)".to_string()),
    }
}

/// Directory holding the mirror clones.
pub(crate) fn cache_dir() -> Result<PathBuf, String> {
    Ok(koja_home()?.join("cache").join("git"))
}

/// Mirror-clone directory for `url`: a readable slug plus a URL hash
/// so distinct URLs never collide.
pub(crate) fn mirror_dir(url: &str) -> Result<PathBuf, String> {
    Ok(cache_dir()?.join(format!("{}-{:016x}.git", slug(url), fnv1a(url))))
}

/// Ensure a mirror clone of `url` exists in the cache, cloning it on
/// first use. Mirror (not bare) so a later fetch updates every ref.
pub(crate) fn ensure_mirror(url: &str) -> Result<PathBuf, String> {
    let dir = mirror_dir(url)?;
    if dir.is_dir() {
        return Ok(dir);
    }
    fs::create_dir_all(dir.parent().expect("mirror dir has a parent"))
        .map_err(|err| format!("cannot create cache directory: {err}"))?;

    let _guard = CacheLock::acquire(&dir)?;
    if dir.is_dir() {
        // Another process cloned while this one waited on the lock.
        return Ok(dir);
    }
    let target = dir.to_string_lossy();
    if let Err(err) = run(&["clone", "--mirror", "--quiet", url, &target], false) {
        fs::remove_dir_all(&dir).ok();
        return Err(format!("cannot clone `{url}`: {err}{}", auth_hint(url)));
    }
    Ok(dir)
}

/// GitHub answers a missing repository with an auth challenge (a
/// private repo and a nonexistent one are indistinguishable from
/// outside), so a credential prompt for a supposedly public repo
/// usually means the URL is misspelled.
fn auth_hint(url: &str) -> &'static str {
    if url.contains("github.com") {
        " (GitHub asks for credentials when a repository does not exist, so check the owner/repo spelling and your access)"
    } else {
        ""
    }
}

/// Update every ref in a mirror clone from its remote.
pub(crate) fn fetch(mirror: &Path, url: &str) -> Result<(), String> {
    let _guard = CacheLock::acquire(mirror)?;
    run(
        &[
            "-C",
            &mirror.to_string_lossy(),
            "fetch",
            "--prune",
            "--quiet",
            "origin",
        ],
        false,
    )
    .map(|_| ())
    .map_err(|err| format!("cannot fetch `{url}`: {err}"))
}

/// Whether the mirror already contains `rev` as a commit.
pub(crate) fn has_commit(mirror: &Path, rev: &str) -> bool {
    run(
        &[
            "-C",
            &mirror.to_string_lossy(),
            "cat-file",
            "-e",
            &format!("{rev}^{{commit}}"),
        ],
        true,
    )
    .is_ok()
}

/// Resolve a manifest ref to a full commit SHA.
///
/// Branches, tags, and the default branch resolve over the network
/// via `ls-remote` (peeled `^{}` lines win for annotated tags). A
/// `rev` resolves against the mirror clone, fetching once if needed,
/// since remotes generally refuse to answer for arbitrary SHAs.
pub(crate) fn resolve_ref(url: &str, reference: &GitRef) -> Result<String, String> {
    match reference {
        GitRef::Branch(branch) => {
            let full = format!("refs/heads/{branch}");
            let refs = ls_remote(url, &[&full])?;
            find_ref(&refs, &full).ok_or_else(|| format!("branch `{branch}` not found on `{url}`"))
        }
        GitRef::DefaultBranch => {
            let output = run(&["ls-remote", url, "HEAD"], false)
                .map_err(|err| format!("cannot reach `{url}`: {err}{}", auth_hint(url)))?;
            parse_ref_lines(&output)
                .into_iter()
                .find(|(_, name)| name == "HEAD")
                .map(|(sha, _)| sha)
                .ok_or_else(|| format!("cannot resolve default branch of `{url}`"))
        }
        GitRef::Rev(rev) => {
            let mirror = ensure_mirror(url)?;
            if !has_commit(&mirror, rev) {
                fetch(&mirror, url)?;
            }
            run(
                &[
                    "-C",
                    &mirror.to_string_lossy(),
                    "rev-parse",
                    "--verify",
                    &format!("{rev}^{{commit}}"),
                ],
                true,
            )
            .map(|sha| sha.trim().to_string())
            .map_err(|_| format!("rev `{rev}` not found in `{url}`"))
        }
        GitRef::Tag(tag) => {
            let full = format!("refs/tags/{tag}");
            let peeled = format!("{full}^{{}}");
            let refs = ls_remote(url, &[&full, &peeled])?;
            find_ref(&refs, &peeled)
                .or_else(|| find_ref(&refs, &full))
                .ok_or_else(|| format!("tag `{tag}` not found on `{url}`"))
        }
    }
}

/// Export the tree at `rev` from a mirror into `dest` (created if
/// missing). Clean source tree, no `.git`.
pub(crate) fn export_tree(mirror: &Path, rev: &str, dest: &Path) -> Result<(), String> {
    let bytes = run_bytes(&[
        "-C",
        &mirror.to_string_lossy(),
        "archive",
        "--format=tar",
        rev,
    ])
    .map_err(|err| format!("cannot export `{rev}`: {err}"))?;
    fs::create_dir_all(dest).map_err(|err| format!("cannot create {}: {err}", dest.display()))?;
    tar::Archive::new(bytes.as_slice())
        .unpack(dest)
        .map_err(|err| format!("cannot extract `{rev}` into {}: {err}", dest.display()))
}

/// `git ls-remote <url> <patterns...>` as `(sha, ref)` pairs.
fn ls_remote(url: &str, patterns: &[&str]) -> Result<Vec<(String, String)>, String> {
    let mut args = vec!["ls-remote", url];
    args.extend(patterns);
    let output = run(&args, false)
        .map_err(|err| format!("cannot reach `{url}`: {err}{}", auth_hint(url)))?;
    Ok(parse_ref_lines(&output))
}

fn parse_ref_lines(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(sha, name)| (sha.to_string(), name.to_string()))
        .collect()
}

fn find_ref(refs: &[(String, String)], name: &str) -> Option<String> {
    refs.iter()
        .find(|(_, ref_name)| ref_name == name)
        .map(|(sha, _)| sha.clone())
}

/// Run git with stdout captured as UTF-8. stdin and stderr stay
/// inherited so auth prompts work, and `quiet` nulls stderr for probes
/// whose failure is an expected outcome (e.g. `cat-file -e`).
fn run(args: &[&str], quiet: bool) -> Result<String, String> {
    let bytes = run_raw(args, quiet)?;
    String::from_utf8(bytes).map_err(|_| "git produced non-UTF-8 output".to_string())
}

fn run_bytes(args: &[&str]) -> Result<Vec<u8>, String> {
    run_raw(args, false)
}

fn run_raw(args: &[&str], quiet: bool) -> Result<Vec<u8>, String> {
    let stderr = if quiet {
        Stdio::null()
    } else {
        Stdio::inherit()
    };
    let output = Command::new("git")
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .output()
        .map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                "`git` binary not found (git dependencies require git on PATH)".to_string()
            }
            _ => format!("cannot run git: {err}"),
        })?;
    if !output.status.success() {
        let subcommand = args
            .iter()
            .find(|arg| !arg.starts_with('-') && **arg != "-C");
        return Err(format!(
            "git {} exited with {}",
            subcommand.unwrap_or(&""),
            output.status
        ));
    }
    Ok(output.stdout)
}

/// Exclusive advisory lock guarding one mirror clone. Released on
/// drop when the file closes.
struct CacheLock {
    _file: File,
}

impl CacheLock {
    fn acquire(mirror: &Path) -> Result<Self, String> {
        let path = mirror.with_extension("lock");
        let file = File::create(&path)
            .map_err(|err| format!("cannot create cache lock {}: {err}", path.display()))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(format!("cannot lock {}", path.display()));
        }
        Ok(Self { _file: file })
    }
}

fn slug(url: &str) -> String {
    let name = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .trim_end_matches(".git");
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "repo".to_string()
    } else {
        cleaned
    }
}

fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_dirs_are_distinct_per_url() {
        let a = slug("https://github.com/koja-lang/postgres");
        assert_eq!(a, "postgres");
        assert_eq!(slug("git@github.com:acme/secret.git"), "secret");
        assert_ne!(
            fnv1a("https://github.com/koja-lang/postgres"),
            fnv1a("https://github.com/acme/postgres")
        );
    }

    #[test]
    fn ref_lines_parse_shas_and_names() {
        let refs = parse_ref_lines("abc123\trefs/tags/v1\ndef456\trefs/tags/v1^{}\n");
        assert_eq!(
            find_ref(&refs, "refs/tags/v1^{}"),
            Some("def456".to_string())
        );
        assert_eq!(find_ref(&refs, "refs/tags/v2"), None);
    }
}
