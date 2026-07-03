//! DWARF debug-info emission. Function-granular: each user-declared
//! function gets a `DISubprogram`, and every instruction in its body
//! carries a `DILocation` at the function's declaration line, so a
//! runtime panic backtrace resolves frames to `file:line: name()`.
//! Synthetic callables (glue, closures, wrappers, intrinsics) stay
//! unattributed, as they have no single source line to point at.
//!
//! Only the object-emitting `compile_*` paths construct a
//! [`DebugInfo`]. The `emit_*_llvm_ir` snapshot paths leave it `None`
//! so their printed IR stays metadata-free and the golden snapshots
//! don't churn.

use std::iter::Peekable;
use std::path::Path;
use std::str::Chars;

use inkwell::context::Context;
use inkwell::debug_info::{
    AsDIScope, DIFile, DIFlags, DIFlagsConstants, DILocation, DISubprogram, DWARFEmissionKind,
    DWARFSourceLanguage, DebugInfoBuilder,
};
use inkwell::module::{FlagBehavior, Module};
use inkwell::values::FunctionValue;
use koja_ir::IRSymbol;

/// Owns the module's [`DebugInfoBuilder`]. The compile unit it mints
/// lives in the module's metadata once created, so we only retain the
/// builder (used to add per-function subprograms and locations).
pub(crate) struct DebugInfo<'ctx> {
    builder: DebugInfoBuilder<'ctx>,
}

impl<'ctx> DebugInfo<'ctx> {
    /// Create the module's debug-info builder + compile unit. Pins the
    /// DWARF version module flag (inkwell stamps "Debug Info Version"
    /// itself) so `dsymutil` / addr2line read the emitted line tables.
    pub(crate) fn new(context: &'ctx Context, module: &Module<'ctx>, app_name: &str) -> Self {
        module.add_basic_value_flag(
            "Dwarf Version",
            FlagBehavior::Warning,
            context.i32_type().const_int(4, false),
        );
        let (builder, _compile_unit) = module.create_debug_info_builder(
            true,
            DWARFSourceLanguage::C,
            app_name,
            ".",
            "koja",
            false,
            "",
            0,
            "",
            DWARFEmissionKind::Full,
            0,
            false,
            false,
            "",
            "",
        );
        Self { builder }
    }

    /// Mint a `DISubprogram` named `name` defined at `file:line` and
    /// attach it to `llvm_function`. Linkage name is left to default to
    /// `name` so addr2line reports the clean name (it prefers
    /// `DW_AT_linkage_name` when present).
    pub(crate) fn attach_subprogram(
        &self,
        llvm_function: FunctionValue<'ctx>,
        name: &str,
        file: &Path,
        line: u32,
    ) {
        let di_file = self.file_for(file);
        let subroutine = self
            .builder
            .create_subroutine_type(di_file, None, &[], DIFlags::ZERO);
        let subprogram = self.builder.create_function(
            di_file.as_debug_info_scope(),
            name,
            None,
            di_file,
            line,
            subroutine,
            false,
            true,
            line,
            DIFlags::ZERO,
            false,
        );
        llvm_function.set_subprogram(subprogram);
    }

    /// Build a `DILocation` at `line` scoped to `subprogram`. The
    /// caller sets it as the builder's current location so every
    /// instruction emitted for the body attributes to that frame.
    pub(crate) fn location_in(
        &self,
        context: &'ctx Context,
        subprogram: DISubprogram<'ctx>,
        line: u32,
    ) -> DILocation<'ctx> {
        self.builder
            .create_debug_location(context, line, 1, subprogram.as_debug_info_scope(), None)
    }

    /// Flush pending metadata. Must run before object emission,
    /// because the builder's own `Drop` finalizes too late (after
    /// `compile_*` has already written the `.o`).
    pub(crate) fn finalize(&self) {
        self.builder.finalize();
    }

    fn file_for(&self, path: &Path) -> DIFile<'ctx> {
        let (directory, filename) = split_path(path);
        self.builder.create_file(&filename, &directory)
    }
}

/// Split a source path into `(directory, filename)` for DWARF. Empty
/// or parentless paths fall back to `.` so the line table still
/// resolves a filename.
fn split_path(path: &Path) -> (String, String) {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    (directory, filename)
}

/// Human-readable name for a function's `DISubprogram`, derived from
/// its mangled [`IRSymbol`]: drop the leading `<package>.` segment and
/// the `$…$` monomorphization args, so `Global.Option_$Int$.unwrap`
/// reads as `Option.unwrap` and `pbt.crash` as `crash` in a backtrace.
pub(crate) fn display_name(symbol: &IRSymbol) -> String {
    let mangled = symbol.mangled();
    let without_package = mangled.split_once('.').map_or(mangled, |(_, rest)| rest);
    strip_generic_args(without_package)
}

fn strip_generic_args(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '_' if chars.peek() == Some(&'$') => {
                chars.next();
                skip_to_dollar(&mut chars);
            }
            '$' => skip_to_dollar(&mut chars),
            _ => out.push(ch),
        }
    }
    out
}

fn skip_to_dollar(chars: &mut Peekable<Chars<'_>>) {
    for inner in chars.by_ref() {
        if inner == '$' {
            break;
        }
    }
}
