//! Symbol naming for impl methods. Stdlib types keep their bare type name
//! to preserve existing intrinsic symbols (e.g. `Int_hash`); user packages
//! are qualified (e.g. `alpha.Config_new`) so two packages with the same
//! type name never collide on a single LLVM symbol.
//!
//! Lifted off `Compiler` in Wave 6.

use expo_ast::identifier::Package;

use crate::lower::ctx::LowerCtx;

/// Convenience wrapper: like [`method_symbol_prefix`] but reads the current
/// module's package from the lowering context. Use at definition sites
/// where the owning package is the one we're currently compiling. Defaults
/// to bare `type_name` when no package is set.
pub fn current_method_symbol_prefix(ctx: &LowerCtx<'_>, type_name: &str) -> String {
    match ctx.package {
        Some(pkg) => method_symbol_prefix(pkg, type_name),
        None => type_name.to_string(),
    }
}

/// Builds the symbol prefix used for an impl method (before the trailing
/// `_{method}` suffix).
pub fn method_symbol_prefix(pkg: &Package, type_name: &str) -> String {
    match pkg {
        Package::Named(name) => format!("{name}.{type_name}"),
        Package::Std | Package::Unresolved => type_name.to_string(),
    }
}
