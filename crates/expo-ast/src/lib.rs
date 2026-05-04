//! Abstract syntax tree, tokens, and source spans for the Expo language.
//!
//! Expo is a statically compiled, GC-free language with Ruby/Elixir-inspired
//! syntax designed for readability and conciseness. Key syntax traits:
//!
//! - Blocks delimited by keywords and `end` (no braces)
//! - No semicolons — newlines are statement separators
//! - `fn` / `priv fn` visibility model
//! - Pattern matching with exhaustive `match` / `when` guards
//! - String interpolation via `#{expr}`
//! - Ownership-based memory without lifetimes or GC
//!
//! See the project-root design documents for full details:
//! - `ROADMAP.md` — phases, feature status, and guiding principles
//! - `MEMORY.md` — ownership, borrowing, and allocation model
//! - `CONCURRENCY.md` — tasks, actors, and runtime design

pub mod ast;
mod debug_print;
pub mod identifier;
pub mod span;
pub mod token;
pub mod types;
pub mod util;

pub use debug_print::format_file;
