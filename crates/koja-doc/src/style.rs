//! Static assets shipped alongside generated documentation:
//! the HexDocs-inspired stylesheet and the hand-rolled fuzzy
//! search bundle. Both are embedded at compile time so the doc
//! generator stays a self-contained library. The driver writes
//! them to disk verbatim during the `write_doc_files` pass.

/// Stylesheet linked from every page (root + per-package).
pub const CSS: &str = include_str!("../templates/style.css");

/// Fuzzy search bundle linked from every page. Reads
/// `search-index.json` (sibling file in the doc output root) and
/// wires up the `<input id="doc-search">` results dropdown plus
/// the `/` focus shortcut.
pub const SEARCH_JS: &str = include_str!("../assets/search.js");
