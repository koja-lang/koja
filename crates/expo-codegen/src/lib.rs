mod calls;
mod compiler;
mod control;
mod drop;
mod enums;
mod expr;
mod generics;
mod hashtable;
mod list;
mod map;
mod ops;
mod set;
mod stmt;
mod structs;
mod types;
mod util;

pub use compiler::{compile, compile_modules};
