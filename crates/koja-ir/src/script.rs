//! Sealed IR for script-mode sources (`koja run <bare-file>`,
//! `koja eval`, REPL fragments) plus the [`lower_script`] entry
//! point that produces them.
//!
//! Where [`crate::IRProgram`] models a user-declared entry function
//! by its stable [`crate::function::IRSymbol`], an `IRScript` carries
//! its body inline: the top-level statements lowered into a single
//! basic block sequence plus the package fragments needed to resolve
//! any helper-function calls. There is no entry-point symbol, because
//! the script *is* the entry point.
//!
//! Backends consume an `IRScript` directly:
//!
//! - The interpreter (`koja-ir-eval`) drives `script.blocks`
//!   through the same instruction walker it uses for an
//!   `IRFunction.blocks`, looking up callees in `script.packages`.
//! - The LLVM backend (`koja-ir-llvm`) emits `script.blocks`
//!   as the body of a host-runtime `main` function and walks
//!   `script.packages` for non-entry function declarations.
//!
//! The shape mirrors a single function's body without leaking an
//! [`IRFunction`] (which carries a name, parameters, and the
//! "user-declared" semantics that scripts deliberately don't have).

use koja_typecheck::CheckedProgram;

use koja_ast::ast::Statement;
use koja_ast::identifier::Identifier;

use crate::constant::IRConstantValue;
use crate::cycle::break_type_cycles;
use crate::elaborate::elaborate_script;
use crate::enum_decl::IREnumDecl;
use crate::error::LowerError;
use crate::function::{IRBasicBlock, IRFunction, IRSourceDef, IRSymbol};
use crate::generics;
use crate::lower::{LowerOutput, lower_body_to_blocks, lower_package};
use crate::merge;
use crate::package::{IRPackage, insert_package_function};
use crate::program::{collect_link_libraries, empty_global_stdlib_package};
use crate::seal;
use crate::struct_decl::IRStructDecl;
use crate::tail_calls::rewrite_tail_calls;
use crate::types::IRType;
use crate::union_decl::discover_unions;
use crate::yield_checks::{insert_yield_checks, insert_yield_checks_in_body};

/// Sealed output of [`lower_script`]'s success path.
///
/// `blocks` is the implicit function's body: the top-level
/// statements of the script source lowered to one or more basic
/// blocks. Today's scope produces exactly one block ending in
/// `IRTerminator::Return`, mirroring `IRFunction.blocks` for a body
/// without control flow.
///
/// `packages` carries the same per-package function fragments that
/// [`crate::IRProgram::packages`] does, so backends can resolve
/// `IRInstruction::Call` callees without revisiting the typecheck
/// registry.
///
/// `link_libraries` mirrors [`crate::IRProgram::link_libraries`]:
/// a deduped, sorted list of `-l<name>` linker library names
/// collected from every `@extern "C"` function declared anywhere in
/// the script's package fragments. Per-function `link_name`
/// overrides stay on the [`IRFunction`].
///
/// `return_type` is the static type of the script's trailing
/// expression value (or `IRType::Unit` for an empty / non-expression
/// trailing statement). Backends consume this directly to size the
/// `main` return slot and the `Return` terminator's value width.
/// `def_location` points at the script's own source file (line 1).
/// The LLVM backend stamps it on the synthesized `__koja_user_main`
/// so a panic in top-level script code resolves to the user's file.
/// `None` for an items-only / synthetic body with no source path.
#[derive(Debug, Clone)]
pub struct IRScript {
    pub blocks: Vec<IRBasicBlock>,
    pub def_location: Option<IRSourceDef>,
    pub link_libraries: Vec<String>,
    pub packages: Vec<IRPackage>,
    pub return_type: IRType,
}

impl IRScript {
    /// Lookup a helper function across every package by its mangled
    /// symbol. Mirrors [`crate::IRProgram::function`] so the
    /// interpreter and LLVM backend can drive a single shared
    /// instruction walker over either IR shape. Only the
    /// call-resolver closure differs.
    pub fn function(&self, mangled: &str) -> Option<&IRFunction> {
        self.packages
            .iter()
            .find_map(|pkg| pkg.functions.get(mangled))
    }

    /// Lookup a struct declaration across every package by its
    /// mangled symbol. Mirrors [`crate::IRProgram::struct_decl`].
    pub fn struct_decl(&self, mangled: &str) -> Option<&IRStructDecl> {
        self.packages
            .iter()
            .find_map(|pkg| pkg.structs.get(mangled))
    }

    /// Lookup an enum declaration across every package by its
    /// mangled symbol. Mirrors [`crate::IRProgram::enum_decl`].
    pub fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl> {
        self.packages.iter().find_map(|pkg| pkg.enums.get(mangled))
    }

    /// Lookup a pooled constant value across every package by its
    /// mangled symbol. Mirrors [`Self::struct_decl`]. Backends pass
    /// the `&IRSymbol` carried on [`crate::IRInstruction::LoadConst`]
    /// directly through the `IRSymbol: Borrow<str>` impl.
    pub fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue> {
        self.packages
            .iter()
            .find_map(|pkg| pkg.constants.get(mangled))
    }
}

/// Lower a script body and its package declarations.
///
/// At most one input file may carry top-level statements. The result
/// runs through package coalescing, generic specialization, cycle
/// breaking, tail-call rewriting, yield insertion, elaboration, and
/// sealing.
pub fn lower_script(checked: &CheckedProgram) -> Result<IRScript, LowerError> {
    let mut output = LowerOutput::default();
    let mut packages: Vec<IRPackage> = Vec::with_capacity(checked.packages.len() + 1);
    packages.push(empty_global_stdlib_package());
    for pkg in &checked.packages {
        packages.push(lower_package(pkg, &checked.registry, &mut output));
    }
    packages = merge::coalesce(packages);

    let body = locate_script_body(checked);
    let body_package = locate_script_body_package(checked);
    let enclosing = body_package.map(synthesize_script_body_symbol);

    let lowered = lower_body_to_blocks(body, enclosing, &checked.registry, &mut output);

    if !output.diagnostics.is_empty() {
        return Err(LowerError::Diagnostics(output.diagnostics));
    }

    let (blocks, return_type) = lowered.unwrap_or_else(|()| {
        panic!(
            "IR lower_script: body lowering returned Err(()) without pushing diagnostics \
             (lower_body_to_blocks contract violation)",
        )
    });

    let synthesized = std::mem::take(&mut output.synthesized_functions);
    if !synthesized.is_empty() {
        let target_package = body_package.unwrap_or_else(|| {
            panic!(
                "IR lower_script: script body produced synthesized closure(s) but no \
                 package owns the body (lower-pass invariant violation)",
            )
        });
        for function in synthesized {
            insert_package_function(&mut packages, target_package, function);
        }
    }

    let initial = std::mem::take(&mut output.instantiations);
    generics::instantiate(
        initial,
        &checked.registry,
        &checked.packages,
        &mut packages,
        &mut output,
    );

    if !output.diagnostics.is_empty() {
        return Err(LowerError::Diagnostics(output.diagnostics));
    }

    let link_libraries = collect_link_libraries(packages.iter());
    let mut script = IRScript {
        blocks,
        def_location: locate_script_body_location(checked),
        link_libraries,
        packages,
        return_type,
    };
    discover_unions(&mut script.packages, &script.blocks);
    break_type_cycles(&mut script.packages);
    rewrite_tail_calls(&mut script.packages);
    insert_yield_checks(&mut script.packages);
    insert_yield_checks_in_body(&mut script.blocks);
    elaborate_script(&mut script.packages, &mut script.blocks);
    seal::seal_script(&script);
    Ok(script)
}

/// Find the populated `File.body` in `checked`, or fall back to an
/// empty slice when no file carries one (a script source that's
/// items-only, e.g. a REPL session before the user types a
/// trailing expression). Panics if more than one file carries a
/// body, since the driver must dispatch script-mode lowering on a
/// single source file.
fn locate_script_body(checked: &CheckedProgram) -> &[Statement] {
    let mut body: Option<&[Statement]> = None;
    for pkg in &checked.packages {
        for file in &pkg.files {
            if let Some(stmts) = file.body.as_deref() {
                if body.is_some() {
                    panic!(
                        "IR lower_script: more than one file carries `File.body` \
                         (the driver must dispatch script-mode lowering on a single source)",
                    );
                }
                body = Some(stmts);
            }
        }
    }
    body.unwrap_or(&[])
}

/// Package name of the (unique) file owning the script body. `None`
/// for an items-only script (every REPL session before the user
/// types a top-level expression). Mirrors [`locate_script_body`]'s
/// scan but returns the package handle so synthesized closures can
/// land alongside the body's own emitted IR.
fn locate_script_body_package(checked: &CheckedProgram) -> Option<&str> {
    for pkg in &checked.packages {
        for file in &pkg.files {
            if file.body.is_some() {
                return Some(pkg.package.as_str());
            }
        }
    }
    None
}

/// Source location of the script body for DWARF: the path of the
/// (unique) file carrying `File.body`, attributed to line 1. `None`
/// when no file owns a body (items-only script) or the body file has
/// no path (in-memory REPL / eval fragment).
fn locate_script_body_location(checked: &CheckedProgram) -> Option<IRSourceDef> {
    for pkg in &checked.packages {
        for file in &pkg.files {
            if file.body.is_some() {
                return file.path.clone().map(|path| IRSourceDef {
                    file: path,
                    line: 1,
                });
            }
        }
    }
    None
}

/// Synthesize the enclosing-symbol root for closures defined inside
/// the script body. Yields `<package>.__script_body`, and child closures
/// then derive `<package>.__script_body__closure<N>` off it.
fn synthesize_script_body_symbol(package: &str) -> IRSymbol {
    IRSymbol::from_identifier(&Identifier::new(package, vec!["__script_body".to_string()]))
}
