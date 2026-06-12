# koja-doc

HTML documentation generator from `@doc` annotations. Package-
aware: every documented item lives under exactly one
[`DocPackage`] and the renderer emits a `doc/<Pkg>/<Item>.html`
tree alongside a root package roster (`doc/index.html`), a
search index, and the shared CSS / JS assets.

## Key files

- `extract.rs` -- Walks the AST, builds package-grouped doc
  structs from `@doc` annotations. `extract_items` takes the
  source's package + origin tier (`PackageKind::Project /
Dependency / Stdlib`) so the driver can mix project, dep,
  and stdlib sources into one project.
- `render.rs` -- Askama templates for the root index, per-
  package index, and per-item pages. Each render call receives
  the sidebar context (`packages`, `sidebar_items`,
  `current_package`, `active_item`, `root_prefix`).
- `search.rs` -- Emits `search-index.json`: one entry per item
  - one per method (deep-linked to `#fn-<name>`). Doubles as
    the AI-friendly bundle.
- `style.rs` -- Embeds `templates/style.css` (CSS) and
  `assets/search.js` (the hand-rolled fuzzy search bundle).
- `templates/` -- `index.html` (root roster), `package_index.html`,
  `item_*.html`, `sidebar.html` (search + dropdown + items),
  `head.html`, `function_detail.html`, `theme_toggle.html`.
- `assets/search.js` -- Self-contained fuzzy search reading
  `search-index.json` from the doc root. `/` focuses,
  ↑↓ + Enter navigate, Esc dismisses.
- `tests/multi_package.rs` -- End-to-end coverage of the
  extract → finalize → render → search-index pipeline.

## Driver integration

[`koja-driver`'s `cmd_doc`](../koja-driver/src/commands.rs)
calls `extract_items` once per (parsed_file, package, kind)
tuple. By default it bundles the project + every path dep +
the embedded stdlib (`koja_stdlib::autoimport_sources()` +
`qualified_sources()`); `--project-only` opts out of the
stdlib + deps. The driver's `cmd_doc_serve` rebuilds (unless
`--no-rebuild`) and then hosts the doc tree via
[`koja-driver`'s `serve` module](../koja-driver/src/serve.rs)
— required for the in-page fuzzy search since browsers refuse
to `fetch()` `search-index.json` over `file://`.
