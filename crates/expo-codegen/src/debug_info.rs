//! DWARF debug info generation via LLVM's `DIBuilder`.
//!
//! `DebugContext` owns all debug metadata state and provides methods to
//! register source files, push/pop function scopes, and set source
//! locations on emitted IR instructions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use inkwell::context::Context;
use inkwell::debug_info::{
    AsDIScope, DICompileUnit, DIFile, DIFlags, DIFlagsConstants, DIScope, DISubprogram,
    DWARFEmissionKind, DWARFSourceLanguage, DebugInfoBuilder,
};
use inkwell::module::Module as LlvmModule;
use inkwell::values::FunctionValue;

/// Owns all LLVM DWARF debug metadata for a compilation session.
pub struct DebugContext<'ctx> {
    builder: DebugInfoBuilder<'ctx>,
    compile_unit: DICompileUnit<'ctx>,
    files: HashMap<PathBuf, DIFile<'ctx>>,
    scope_stack: Vec<DIScope<'ctx>>,
    /// Saved (line, column) for each pushed scope so `pop_scope` can restore
    /// the caller's debug location automatically.
    location_stack: Vec<(u32, u32)>,
    current_loc: (u32, u32),
    current_file: Option<DIFile<'ctx>>,
}

impl<'ctx> DebugContext<'ctx> {
    /// Creates a new debug context, initializing the `DIBuilder` and
    /// `DICompileUnit` for the given LLVM module.
    pub fn new(
        llvm_module: &LlvmModule<'ctx>,
        filename: &str,
        directory: &str,
        release: bool,
    ) -> Self {
        let emission = if release {
            DWARFEmissionKind::LineTablesOnly
        } else {
            DWARFEmissionKind::Full
        };

        let (builder, compile_unit) = llvm_module.create_debug_info_builder(
            true,
            DWARFSourceLanguage::C,
            filename,
            directory,
            "expo",
            false,
            "",
            0,
            "",
            emission,
            0,
            false,
            false,
            "",
            "",
        );

        let scope = compile_unit.as_debug_info_scope();

        Self {
            builder,
            compile_unit,
            files: HashMap::new(),
            scope_stack: vec![scope],
            location_stack: Vec::new(),
            current_loc: (0, 0),
            current_file: None,
        }
    }

    /// Registers a source file for debug info. Returns the `DIFile` handle.
    pub fn register_file(&mut self, path: &Path) -> DIFile<'ctx> {
        if let Some(file) = self.files.get(path) {
            return *file;
        }

        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("unknown");
        let directory = path.parent().and_then(|p| p.to_str()).unwrap_or(".");

        let file = self.builder.create_file(filename, directory);
        self.files.insert(path.to_path_buf(), file);
        file
    }

    /// Sets the current file scope for subsequent function/location emissions.
    pub fn set_current_file(&mut self, path: &Path) {
        self.current_file = self.files.get(path).copied();
    }

    /// Returns the current file, falling back to the compile unit file.
    pub fn file(&self) -> DIFile<'ctx> {
        self.current_file
            .unwrap_or_else(|| self.compile_unit.get_file())
    }

    /// Returns the current innermost debug scope.
    pub fn current_scope(&self) -> DIScope<'ctx> {
        *self.scope_stack.last().expect("debug scope stack is empty")
    }

    /// Creates a `DISubprogram` for a function, attaches it to the LLVM
    /// function value, and pushes it onto the scope stack.
    pub fn push_function(
        &mut self,
        fn_value: FunctionValue<'ctx>,
        name: &str,
        linkage_name: &str,
        file: DIFile<'ctx>,
        line: u32,
    ) -> DISubprogram<'ctx> {
        let subroutine_type = self
            .builder
            .create_subroutine_type(file, None, &[], DIFlags::PUBLIC);

        let subprogram = self.builder.create_function(
            file.as_debug_info_scope(),
            name,
            Some(linkage_name),
            file,
            line,
            subroutine_type,
            true,
            true,
            line,
            DIFlags::PUBLIC,
            false,
        );

        fn_value.set_subprogram(subprogram);
        self.location_stack.push(self.current_loc);
        self.scope_stack.push(subprogram.as_debug_info_scope());
        subprogram
    }

    /// Pops the most recent function scope and restores the caller's debug
    /// location on the IR builder.
    pub fn pop_scope(
        &mut self,
        context: &'ctx Context,
        ir_builder: &inkwell::builder::Builder<'ctx>,
    ) {
        if self.scope_stack.len() > 1 {
            self.scope_stack.pop();
            if let Some((line, col)) = self.location_stack.pop()
                && self.scope_stack.len() > 1
            {
                self.set_location(context, ir_builder, line, col);
            }
        }
    }

    /// Sets the current debug location on the IR builder for subsequent
    /// instructions.
    pub fn set_location(
        &mut self,
        context: &'ctx Context,
        ir_builder: &inkwell::builder::Builder<'ctx>,
        line: u32,
        column: u32,
    ) {
        self.current_loc = (line, column);
        let scope = self.current_scope();
        let loc = self
            .builder
            .create_debug_location(context, line, column, scope, None);
        ir_builder.set_current_debug_location(loc);
    }

    /// Finalizes all debug info descriptors. Must be called before LLVM
    /// module verification.
    pub fn finalize(&self) {
        self.builder.finalize();
    }
}
