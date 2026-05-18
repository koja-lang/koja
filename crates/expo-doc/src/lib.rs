//! HTML documentation generator for Expo source files.
//!
//! Walks each `@doc`-annotated declaration in the parsed AST and
//! emits a `doc/<Pkg>/<Item>.html` tree alongside a root-level
//! package roster (`doc/index.html`), an external stylesheet
//! (`doc/style.css`), a hand-rolled fuzzy search bundle
//! (`doc/search.js`), and a search index (`doc/search-index.json`)
//! that doubles as an AI-readable bundle of the surface.
//!
//! Drivers wire it up by repeatedly calling [`extract_items`] for
//! each `(parsed_file, package_name, kind)` tuple, then
//! [`finalize_project`] once, then the various `render_*` calls
//! to produce HTML. [`search::search_index_json`] produces the
//! search payload; [`style::CSS`] / [`style::SEARCH_JS`] expose
//! the static assets that ship alongside the HTML.

mod extract;
mod render;
mod search;
mod style;

pub use extract::{
    DocConstant, DocEnum, DocFunction, DocItem, DocPackage, DocProject, DocProtocol, DocStruct,
    PackageKind, extract_items, finalize_project,
};
pub use render::{
    render_constant, render_enum, render_function, render_package_index, render_protocol,
    render_root_index, render_struct,
};
pub use search::search_index_json;
pub use style::{CSS, SEARCH_JS};
