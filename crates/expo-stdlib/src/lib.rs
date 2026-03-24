//! Embedded standard library sources for the Expo language.
//!
//! Each module is available as a `(&str, &str)` pair of `(module_name, source)`.
//! [`SOURCES`] provides all modules in dependency order (kernel first).

pub const KERNEL: &str = include_str!("../std/kernel.expo");
pub const LIST: &str = include_str!("../std/list.expo");
pub const STRING: &str = include_str!("../std/string.expo");
pub const MAP: &str = include_str!("../std/map.expo");
pub const SET: &str = include_str!("../std/set.expo");
pub const BITWISE: &str = include_str!("../std/bitwise.expo");
pub const FD: &str = include_str!("../std/fd.expo");

/// All stdlib sources in dependency order. Kernel must come first;
/// subsequent modules may reference types defined by earlier ones.
///
/// Each entry is `(fully_qualified_module_name, source_text)`.
pub const SOURCES: &[(&str, &str)] = &[
    ("std.kernel", KERNEL),
    ("std.list", LIST),
    ("std.string", STRING),
    ("std.map", MAP),
    ("std.set", SET),
    ("std.bitwise", BITWISE),
    ("std.fd", FD),
];
