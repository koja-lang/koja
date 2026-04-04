# Code Distribution: Cookbook, Not Packages

Design notes for how Expo distributes reusable code. The traditional
package model (registry, versioning, dependency resolution) assumes
humans copy exact code into their projects. AI code generation
changes that assumption. This document proposes an alternative.

---

## The problem with packages

### For a closed-source language

Expo is closed-source. Every first-party package must be too. Git-based
distribution requires SSH keys or API tokens for private repos. Every
developer who wants to use `expo-http` needs credentials configured.
CI pipelines need deploy keys. Onboarding friction compounds with each
dependency.

Open-source packages from the community don't exist yet (the language
is new), and when they do, they need a discovery mechanism. Building a
package registry (hex.pm, crates.io) is a massive infrastructure
investment with ongoing maintenance.

### For the AI era

AI code generation changes what "using a library" means. When a
developer says "add a Postgres client," the AI doesn't need to:

1. Search a registry for a package
2. Add it to a manifest
3. Resolve version constraints
4. Download and link the dependency

It needs to:

1. Read a reference implementation
2. Understand the wire protocol and API patterns
3. Generate a client tailored to the project's types and conventions

The output is code the developer owns. No dependency relationship. No
version to pin. No breaking changes to track.

### Versioning is unnecessary

Traditional packages need semantic versioning because your binary
links against their binary. The API contract must be exact. If
`postgres` changes `connect` to `open`, your build breaks.

When AI generates code from a reference implementation, there's no
linking. The AI adapts. If the reference renames a function, the AI
reads the new version and generates accordingly. Nothing breaks
because there's no dependency -- only reference material.

---

## The cookbook model

A single git repository (`expo-cookbook`) containing well-written,
tested, documented Expo implementations of common tasks.

```
expo-cookbook/
  postgres/
    connect.expo          -- TCP connect, startup message, auth
    query.expo            -- simple query protocol
    prepared.expo         -- prepared statements
    types.expo            -- OID to Expo type mapping
    README.md             -- protocol documentation

  websocket/
    handshake.expo        -- HTTP upgrade
    frame.expo            -- frame encode/decode
    client.expo           -- full client example

  btree/
    node.expo             -- on-disk node layout
    insert.expo           -- insertion with splitting
    search.expo           -- key lookup

  redis/
    client.expo           -- RESP protocol, connection, commands

  patterns/
    rest_api.expo         -- HTTP server with JSON routes
    worker_pool.expo      -- supervised process pool
    rate_limiter.expo     -- token bucket with processes
    cache.expo            -- TTL cache as a process
```

### What's in the cookbook vs. what's in stdlib

**Stdlib** -- primitives that are exact and universal. `std.socket`,
`std.file`, `std.time`. Building blocks that the runtime provides and
every program may need. Stable API, ships with the compiler.

**Cookbook** -- compositions of stdlib primitives for specific tasks.
"Here's how to implement the Postgres wire protocol using
`std.socket` and binary pattern matching." The cookbook uses stdlib but
is not stdlib.

The litmus test: if the implementation involves a third-party
protocol, format, or algorithm that not every program needs, it's a
cookbook entry.

### Contribution model

Open source, even though the language is closed-source. Cookbook
entries are `.expo` files -- they're source text, not compiler
internals. Anyone can read and contribute.

1. Someone writes a Redis client in Expo
2. They submit an MR to `expo-cookbook`
3. It gets reviewed for correctness, style, and documentation quality
4. It's merged and auto-published to the docs site
5. No release process, no semver, no changelog per entry

The barrier to contribution is writing one good `.expo` file with
doc comments. Not maintaining a library with issues, releases, and
backwards compatibility.

### Relationship to stdlib and `expo.toml`

Stdlib, the cookbook, and `expo.toml` dependencies serve different
purposes:

| Mechanism        | Purpose                          | Ships with compiler | Versioned           |
| ---------------- | -------------------------------- | ------------------- | ------------------- |
| `std.*`          | Language primitives              | Yes                 | Yes (with compiler) |
| Cookbook         | Reference implementations        | No (separate repo)  | No                  |
| `expo.toml` deps | Shared code between own projects | N/A                 | By git ref          |

`expo.toml` local path and git dependencies remain useful for sharing
code between your own projects (a monorepo with shared types, an
internal library used across services). The cookbook doesn't replace
that -- it replaces the _community package ecosystem_.

---

## The docs site

### `expo doc` as the unified interface

`expo doc` already generates HTML for stdlib. Extending it to also
generate pages for cookbook entries creates a single browsable site
that serves as:

- **API reference** for stdlib (already working)
- **Discovery and browsing** for cookbook entries (new)
- **AI context source** for code generation (new)

The experience resembles hex.pm or crates.io -- search, browse by
category, read docs for any entry -- but there's nothing to download.
The "installation" step is "AI reads the source and writes your
version."

### Dual-audience HTML

Each cookbook entry page serves two audiences simultaneously:

**Humans** see clean documentation: entry name, description, function
signatures, doc comments, usage examples. Source code is collapsed by
default.

**AI tools** fetch the same page and get full source code in the HTML
DOM. No separate repo to clone, no file traversal needed.

Implementation uses standard HTML `<details>` tags:

```html
<h3>Postgres.connect</h3>
<p>
  Establishes a connection to a PostgreSQL server using the v3 wire protocol.
</p>

<pre><code>conn = Postgres.connect("localhost", 5432, "mydb", "user", "pass")</code></pre>

<details>
  <summary>Source</summary>
  <pre><code>
  fn connect(host: String, port: Int, ...) -> Result&lt;Connection, PgError&gt;
    sock = Socket.connect(host, port)
    // ... full implementation ...
  end
  </code></pre>
</details>
```

`<details>` is standard, semantic HTML. Humans can expand it if they
want. AI tools (web fetchers, crawlers) get the full content from the
DOM. Screen readers see it. No JavaScript required.

### Site structure

```
docs.expo-lang.dev/

  std/                        -- stdlib reference
    String
    List
    Option
    Socket
    ...

  cookbook/                    -- community entries
    postgres
    redis
    websocket
    btree
    ...
```

Two top-level sections. Stdlib is organized by type (existing `expo
doc` output). Cookbook entries are organized by name, each with its own
page containing description, API surface, examples, and collapsible
full source.

Search covers both sections. A developer searching "socket" finds
`std.socket` (the primitive) and cookbook entries that use it (TCP
server patterns, WebSocket client).

### Hosting

GitHub Pages, built by CI on merge to `main`. The cookbook repo
contains `.expo` source files and metadata. CI runs `expo doc` (or
an extended variant) to generate the static site. No server, no
database, no infrastructure to maintain.

---

## Developer workflow

### Building something new

1. Developer: "I need a Postgres client"
2. Goes to docs.expo-lang.dev, searches "postgres"
3. Finds the Postgres cookbook entry with docs, examples, full source
4. Either:
   - Tells AI: "Build me a Postgres client, reference this page"
   - Reads the source and adapts it manually
5. Code lives in `src/`. It's theirs. No dependency.

### Updating existing code

1. Six months later, the Postgres cookbook entry improves (better error
   handling, connection pooling, new auth method)
2. Developer tells AI: "Check the latest Postgres examples and update
   my client"
3. AI fetches the page, compares with existing code, suggests changes
4. Still no dependency. Still their code.

### Contributing

1. Developer builds something useful (a MessagePack encoder, a rate
   limiter, an S3 client)
2. Extracts the implementation into clean, well-documented `.expo` files
3. Submits an MR to `expo-cookbook`
4. After review: merged, CI rebuilds the site, entry is live

---

## Sharp edges

### Protocol correctness

Cookbook entries for wire protocols (Postgres, Redis RESP, WebSocket
framing, SCRAM-SHA-256 auth) are specification-exact code. A subtle
bug in authentication or frame parsing isn't "close enough." For
these entries:

- The source should be treated as a reference implementation, not
  loose inspiration
- AI should copy near-verbatim, adapting only types and error handling
- Tests in the cookbook repo validate protocol correctness
- Security-sensitive entries (crypto, auth) get extra review scrutiny

### Patterns vs. implementations

Not everything in the cookbook is protocol code. Two categories:

**Reference implementations** -- exact protocol/algorithm code (Postgres
wire protocol, B-tree insertion, WebSocket framing). AI should preserve
the logic faithfully.

**Patterns** -- architectural templates (REST API structure, worker pool,
rate limiter, cache). AI should adapt creatively to the project's
specific needs.

The cookbook's documentation should make the distinction clear for each
entry.

### Staleness

Without versioning, how do entries stay current? The same way any open
source project stays current: active maintainership and community
contributions. If an entry gets stale, someone submits a better one.
The barrier is low (one MR with `.expo` files), so the cost of
replacement is low.

The absence of backwards compatibility concerns actually helps here.
Replacing a stale Postgres implementation doesn't break anyone's
build. The old consumers have their own generated code that continues
to work.

---

## What this replaces in the roadmap

The roadmap currently lists:

- **Package manager**: git dependencies, lock file, `alias` keyword
- **First-party packages**: `net`, `http`, `websocket`, `json`,
  `crypto`, structured logging, MessagePack, UUID, regex, URL parsing

Under the cookbook model:

- **Package manager**: reduced scope. `expo.toml` `[deps]` with local
  paths (already working) and git URLs remain for sharing code between
  your own projects. No public registry, no lock file, no version
  resolution.
- **`alias` keyword**: still useful for shortening qualified types
  from git dependencies between your own projects.
- **First-party packages**: become first-party cookbook entries. Same
  code quality, same team authorship, but distributed as reference
  material rather than linkable dependencies.
- **C FFI**: unchanged. Still needed for crypto libraries, system TLS,
  database drivers that wrap C code. The cookbook entry for `crypto`
  would include the FFI wrapper source.

---

## Design pressure

The cookbook site doubles as a design tool for the language author.
Browsing stdlib and cookbook entries side by side reveals:

- When a cookbook entry uses a pattern that should be in stdlib
- When two entries duplicate logic that deserves extraction
- Structural inconsistencies in naming, error handling, API shape
- Gaps in stdlib that force cookbook entries into awkward workarounds

This is the value of browsing over searching. AI answers questions;
browsing surfaces questions you didn't know to ask.

---

## Summary

1. **No package registry.** No hex.pm equivalent, no version
   resolution, no dependency graph.
2. **Cookbook repo.** Community-contributed reference implementations
   in a single git repository. Open source, even though the language
   is closed-source.
3. **`expo doc` as the site.** Extended to generate pages for cookbook
   entries alongside stdlib. Browsable, searchable, like a registry
   without the registry.
4. **Dual-audience HTML.** `<details>` tags make source code
   accessible to AI tools while keeping pages clean for humans.
5. **No versioning.** Entries improve continuously. Nobody's build
   breaks because there are no dependencies -- only reference
   material.
6. **`expo.toml` deps remain.** For sharing code between your own
   projects via local paths and git URLs. Not for community
   distribution.
7. **Contribution via MR.** Low barrier (one `.expo` file), no
   maintenance burden (no releases, no backwards compatibility).

The model: distribute knowledge, not code. The AI is the translator
between reference implementations and project-specific code.
