//! Static assets shipped alongside generated documentation. The
//! stylesheet, page-chrome script, fuzzy search bundle, and
//! self-hosted fonts are all embedded at compile time so the doc
//! generator stays a self-contained library and generated docs
//! work offline. The driver writes them to disk verbatim during
//! the `write_doc_files` pass.

/// Stylesheet linked from every page (root + per-package).
pub const CSS: &str = include_str!("../templates/style.css");

/// Page chrome for the theme toggle, mobile rail toggle, and
/// right-TOC scroll spy. Linked from every page.
pub const DOC_JS: &str = include_str!("../assets/doc.js");

/// Fuzzy search bundle linked from every page. Reads
/// `search-index.json` (sibling file in the doc output root) and
/// wires up the `<input id="doc-search">` results dropdown plus
/// the `/` focus shortcut.
pub const SEARCH_JS: &str = include_str!("../assets/search.js");

/// Self-hosted woff2 fonts (latin subsets), written to `fonts/` in
/// the doc output root. The same families the website uses, with
/// Outfit for headings, Source Sans 3 for body, and Source Code
/// Pro for code.
pub const FONTS: &[(&str, &[u8])] = &[
    (
        "outfit.woff2",
        include_bytes!("../assets/fonts/outfit.woff2"),
    ),
    (
        "source-sans-3.woff2",
        include_bytes!("../assets/fonts/source-sans-3.woff2"),
    ),
    (
        "source-code-pro.woff2",
        include_bytes!("../assets/fonts/source-code-pro.woff2"),
    ),
];
