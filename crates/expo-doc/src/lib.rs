//! HTML documentation generator for Expo source files.
//!
//! Extracts `@doc` annotations from the parsed AST and renders
//! HexDocs-inspired static HTML pages.

mod extract;
mod render;
mod style;

pub use extract::{DocProject, extract_items, finalize_project};
pub use render::{
    render_constant, render_enum, render_function, render_index, render_protocol, render_struct,
};
