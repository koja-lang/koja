//! HTML documentation generator for Expo source files.
//!
//! Extracts `@doc` and `@moduledoc` annotations from the parsed AST and
//! renders HexDocs-inspired static HTML pages.

mod extract;
mod render;
mod style;

pub use extract::{DocModule, extract_module};
pub use render::{render_index, render_module};
