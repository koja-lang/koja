//! Per-function FFI metadata for `@extern "C"` declarations:
//! the optional `link_name` (C symbol override) and `link_lib`
//! (linker library) parsed at lower time from the AST annotations.
//!
//! `IRExternAttrs` is the IR-layer composition of two pieces of AST
//! metadata ([`expo_ast::ast::is_extern_c`] for the ABI marker plus
//! [`expo_ast::ast::AnnotationKind::Link`] for the linker payload);
//! the AST keeps both pieces deliberately separate because `@link`
//! is pure linker metadata (no ABI implication) and `@extern "C"`
//! is pure ABI metadata (no library name). They only fuse here,
//! where the IR's [`crate::FunctionKind::Extern`] needs both to
//! direct the LLVM declare and the program-level link arg list.

use expo_ast::ast::{Annotation, AnnotationKind};

/// Extern function attributes derived from a function's
/// `@extern "C"` + `@link` annotations.
///
/// `link_name` overrides the C symbol the function resolves to at
/// link time (the `sym` half of `@link "lib:sym"`). When `None`
/// the LLVM backend uses the function's bare last-segment name
/// (`fn cosf` → `cosf`).
///
/// `link_lib` is the bare library name (`@link "m"` → `m`) the
/// driver feeds to `cc -l<name>`. Multiple `@extern "C"` functions
/// can name the same library — the IR layer (in [`crate::IRProgram`]
/// / [`crate::IRScript`]) dedupes across the program before
/// surfacing a sorted list to the driver.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IRExternAttrs {
    pub link_name: Option<String>,
    pub link_lib: Option<String>,
}

impl IRExternAttrs {
    /// Build an [`IRExternAttrs`] from a function's `annotations`.
    /// Caller is responsible for already having checked
    /// [`expo_ast::ast::is_extern_c`] — this helper just collects
    /// the optional `@link` payload(s).
    ///
    /// Multiple `@link` annotations on one function fold with
    /// last-write-wins for whichever fields each one carries.
    /// Annotations whose `kind()` isn't [`AnnotationKind::Link`] are
    /// skipped silently — the typecheck layer is responsible for
    /// rejecting unrecognized annotations.
    pub fn from_annotations(annotations: &[Annotation]) -> Self {
        let mut attrs = Self::default();
        for a in annotations {
            if let AnnotationKind::Link { lib, name } = a.kind() {
                if let Some(l) = lib {
                    attrs.link_lib = Some(l.to_string());
                }
                if let Some(n) = name {
                    attrs.link_name = Some(n.to_string());
                }
            }
        }
        attrs
    }
}
