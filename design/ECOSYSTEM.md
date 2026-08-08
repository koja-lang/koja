# Package Ecosystem: Index, Not Registry

How Koja distributes, discovers, and documents community code. This
doc argues a position: git stays the source of truth, and all central
infrastructure is read-only. It supersedes the registry direction in
`ROADMAP.md` and completes the rethink that
`archive/20260610-PACKAGE.md` started.

## Goals

The ecosystem needs four things:

1. A mechanism to host dependency libraries.
2. Hosted docs for every package, without asking maintainers to build
   them.
3. Package search.
4. Credibility signals for packages and their maintainers.

Git already solves goal 1, and `koja deps get` with a committed
`koja.lock` already consumes it. The other three goals need
infrastructure, but not a registry.

## The decision

No hex.pm-style central registry. No accounts, no upload endpoint, no
custody of anyone's code. Instead:

- **Git is the source of truth.** A package is a GitHub repo with
  semver tags. Publishing a release is pushing a tag.
- **A crawler builds the ecosystem surface.** It finds opted-in
  repos, indexes their metadata, generates `koja doc` output for
  every tagged version, and publishes a static docs-and-search site.
- **A caching fetch proxy is a separable later layer.** It adds
  immutability, availability, and download counts. Nothing in the
  crawler design blocks it or depends on it.

Every registry feature reduces to a file in a git repo that the
crawler reads. The only central components are a static site and,
later, a cache.

## Sizing: why not a registry

The AI era killed the long tail of packages, not the head. Nobody
needs a dependency for a rate limiter or a UUID formatter when
generated code fits the project better. What survives is the
infrastructure tier: drivers, protocol stacks, codecs, crypto, and
framework-shaped libraries. For Koja that head is realistically one
to two hundred maintained packages, mirroring the load-bearing core
of hex or crates.io once the substitutes and abandonware are removed.

At that scale, search is a directory, cred is legible at a glance,
and a curated head ("everything you need exists and is maintained")
is a stronger pitch than raw package counts. Registry-grade
infrastructure would be sized for a problem Koja will not have.

The pattern tier still matters, but as knowledge rather than
artifacts. It lives in the cookbook (see below), which absorbs the
useful half of `archive/20260610-PACKAGE.md`.

## Discovery and consent

A repo is indexed when both hold:

- The repo carries the `koja` GitHub topic.
- The repo root contains a `koja.toml`.

The topic is the discovery signal, since the crawler can query GitHub
for it. The manifest is the consent and metadata channel. Forks are
excluded. De-listing is removing the topic, or setting `index =
false` in the manifest.

The manifest gains an `[index]` section:

```toml
[project]
name = "Postgres"
koja = "0.16.0"

[index]
description = "PostgreSQL driver with connection pooling"
categories = ["database"]
keywords = ["postgres", "sql"]
kind = "library" # or "application"
# index = false opts out entirely
```

Rules:

- `categories` come from a small curated slug list that lives in the
  crawler repo and grows by PR. Unknown slugs warn at index time.
  `keywords` are free-form and feed search only.
- `kind = "library"` lists the package in search. `kind =
"application"` indexes it into a separate showcase, not the
  dependency listing. Default is library when semver tags exist.
- **Living metadata reads from the default branch, artifact metadata
  reads from tags.** Opt-out and advisories must take effect without
  cutting a release. Description and categories should match what a
  given version ships.

## Identity and naming

A package is identified as `owner/repo`. The scope is the GitHub
account, npm-style, so name squatting is delegated to GitHub's
account system and Koja never runs a naming bureaucracy.

The `[project]` name (`Postgres`) stays what it is today: the
language-level namespace consumers qualify against. Two repos may
both declare `Postgres`. That is legal, visible in search results,
and resolved by the consumer's choice of `owner/repo` in their
dependency table. The registry-level namespace is scoped and unique.
The language-level namespace stays flat, as the language already
works.

## Versions

Versions are semver git tags. The crawler indexes each new tag it
sees on its polling pass. Untagged repos may be listed but are marked
unversioned and get no docs builds. Nothing changes about `koja deps
get`: it still pins exact commits into `koja.lock`.

## The docs and search site

One static site, built by the crawler, with three sections:

- **Stdlib reference.** The existing `koja doc` output for the
  compiler's embedded stdlib.
- **Package docs.** For every indexed package and every semver tag,
  the crawler clones the repo and runs `koja doc` in a sandboxed
  container, then publishes the HTML under `owner/repo/version`. Doc
  generation parses rather than executes, but the input is untrusted,
  so it runs containerized anyway. Maintainers do nothing to get
  docs. This is the hexdocs experience without the publish step.
- **Cookbook.** PR-contributed reference implementations and patterns
  (worker pools, caches, protocol walkthroughs), rendered as
  dual-audience pages: clean docs for humans, full source in
  collapsible `<details>` blocks so AI tools get complete context
  from the DOM. Protocol-exact entries are labeled as reference
  implementations. Architectural entries are labeled as patterns to
  adapt.

Search covers all three sections.

## Credibility signals

Stars, freshness, and structure. No download counts in this phase.

- **GitHub stars**, passed through from the crawl.
- **Freshness**: date of the latest release, and whether recent
  compiler versions build the package.
- **Imported-by counts**, computed from the dependency tables of
  every indexed `koja.toml`. Structural, hard to fake, and the signal
  that actually helps a consumer choose between two drivers.

Download counts require a fetch chokepoint, so they arrive with the
proxy layer. They mostly serve language marketing rather than package
selection, which is why they can wait.

## Advisories and retirement

Each repo owns its security record in an `advisories.toml` on its
default branch:

```toml
[[retired]]
versions = "0.3.*"
reason = "security" # or deprecated, invalid

[[advisory]]
id = "PG-2026-001"
affected = "< 0.4.2"
severity = "high"
summary = "SCRAM nonce validation skipped on resumed connections"
```

This decentralizes hex's version retirement: the same commit rights
that publish a version can retire it, and no release is needed to
warn consumers. Tooling behavior:

- The crawler banners retired and advisory-affected versions on the
  docs site, and its cache preserves advisory knowledge even if the
  repo is later deleted or de-indexed.
- `koja deps get` warns when the lockfile pins an affected version.
- `koja audit` checks a lockfile against all known advisories, for
  CI.

For unresponsive or compromised maintainers, a community
`koja-advisories` overlay repo (the RustSec model: TOML files in git,
merged by PR) provides third-party advisories. Tooling merges both
sources. Neither requires a service.

## The fetch proxy (later layer)

When the package tier shows a pulse, a caching proxy in front of git
fetches adds the registry-grade guarantees:

- **Immutability and availability.** Every fetched version is cached
  forever, so deleted repos and moved tags cannot break builds. This
  is the left-pad defense.
- **Download counts.** `koja deps get` defaults to fetching through
  the proxy (opt out via `KOJA_PROXY=direct`), so CDN logs yield
  npm-style per-package download counts. The default matters: an
  opt-in proxy is Go's fate, a great cache with unpublishable
  numbers. Ship a short privacy note (aggregate counts only) from day
  one.
- The crawler's index doubles as the proxy's allowlist.

Go proved the model and then declined the optics. The proxy sees
every fetch, and Google chose not to publish per-module counts. Koja
would simply not decline.

## Compiler surface

The pieces that land in this repo, all well-bounded:

- Parse and validate the `[index]` manifest section.
- Parse `advisories.toml`, warn from `koja deps get`, add `koja
audit`.
- No changes to dependency resolution, lockfiles, or `alias`.

The crawler, the site, and the overlay repo live outside the compiler
repo.

## Open questions

- Non-GitHub hosts. The index schema keys on (host, owner, repo,
  path) so GitLab or Codeberg support is additive, but the discovery
  convention (topics) is GitHub-specific today.
- Monorepos with multiple packages. Punted for v1, held open by the
  path component in the index key.
- Whether the dependency table should allow renaming a language-level
  namespace at the consumer side (for collisions between two repos
  declaring the same `[project]` name), or whether choosing one dep
  is enough.
- Advisory ID scheme and severity vocabulary, once `koja audit`
  exists.
