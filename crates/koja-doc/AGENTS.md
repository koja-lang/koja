# koja-doc

HTML documentation generator from `@doc` annotations. Package-aware: every
documented item lives under exactly one [`DocPackage`] and the renderer
emits a `doc/<Pkg>/<Item>.html` tree alongside a root package roster
(`doc/index.html`), a search index, and the shared CSS / JS assets.

## Key files

- `extract.rs`: walks the AST, builds package-grouped doc structs from
  `@doc` annotations. `extract_items` takes the source's package + origin
  tier (`PackageKind::Project / Dependency / Stdlib`) so the driver can mix
  project, dep, and stdlib sources into one project.
- `render.rs`: Askama templates for the root index, per-package index, and
  per-item pages. Each render call builds a `PageContext` (package roster,
  grouped rail items, conditional "on this page" TOC, `root_prefix`).
- `highlight.rs`: hand-rolled Koja highlighter for signatures and
  doc-comment code blocks. Token classes mirror the website's Rouge lexer
  (`kojalang.org/_plugins/koja_lexer.rb`).
- `search.rs`: emits `search-index.json`, one entry per item plus one per
  method (deep-linked to `#fn-<name>`). Doubles as the AI-friendly bundle.
  Also owns the crate-internal `Symbol` enumeration that `terminal.rs`
  shares.
- `terminal.rs`: `koja doc search` backend. Matches a query against every
  symbol and renders plain markdown: an exact name hit prints the full doc
  (signatures via `DocFunction::signature_text()`), anything else prints a
  match list.
- `style.rs`: embeds `templates/style.css`, `assets/doc.js` (theme toggle,
  mobile rail, scroll spy), `assets/search.js` (fuzzy search), and the
  self-hosted woff2 fonts under `assets/fonts/`.
- `templates/`: `index.html` (root roster), `package_index.html`,
  `item_*.html`, `header.html` (brand + package dropdown + search + theme
  toggle), `sidebar.html` (left rail), `toc.html` (right column),
  `head.html`, `function_detail.html`.
- `assets/search.js`: self-contained fuzzy search reading
  `search-index.json` from the doc root. `/` focuses, ↑↓ + Enter navigate,
  Esc dismisses.
- `tests/multi_package.rs`: end-to-end coverage of the
  extract → finalize → render → search-index pipeline.

The page design mirrors kojalang.org: same fonts (Outfit / Source Sans 3 /
Source Code Pro), teal accents, and Ayu Mirage code panels that stay dark
in both themes. Layout is a centered three-column frame (left rail, ~52rem
article, right "on this page" TOC) that is identical on every page. Pages
with no TOC entries leave the third column blank.

## Driver integration

[`koja-driver`'s `cmd_doc`](../koja-driver/src/commands.rs) calls
`extract_items` once per (parsed_file, package, kind) tuple. By default it
bundles the project + every path dep + the embedded stdlib
(`koja_stdlib::autoimport_sources()` + `qualified_sources()`), and
`--project-only` opts out of the stdlib + deps. The driver's
`cmd_doc_serve` rebuilds (unless `--no-rebuild`) and then hosts the doc
tree via [`koja-driver`'s `serve` module](../koja-driver/src/serve.rs).
Serving is required for the in-page fuzzy search since browsers refuse to
`fetch()` `search-index.json` over `file://`. `cmd_doc_search` skips disk
output entirely and prints `terminal::search` results to stdout.

Outside a project (no `koja.toml`), all three commands fall back to
stdlib-only inputs. Generation then defaults its output to
`$TMPDIR/koja-stdlib-doc-<version>` unless `-o` is passed, and
`--project-only` is an error.
