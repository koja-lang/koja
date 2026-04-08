//! Embedded standard library sources for the Expo language.
//!
//! Sources live in `expo/lib/` as proper Expo projects. The build script
//! discovers all `.expo` files and generates the constants, SOURCES array,
//! and QUALIFIED_MODULES list automatically.

include!(concat!(env!("OUT_DIR"), "/stdlib_gen.rs"));
