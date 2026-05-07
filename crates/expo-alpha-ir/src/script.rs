//! Sealed IR for script-mode sources (`expo run <bare-file>`,
//! `expo eval`, REPL fragments) plus the [`lower_script`] entry
//! point that produces them.
//!
//! Where [`crate::IRProgram`] models a user-declared entry function
//! by its stable [`crate::function::IRSymbol`], an `IRScript` carries
//! its body inline: the top-level statements lowered into a single
//! basic block sequence plus the package fragments needed to resolve
//! any helper-function calls. There is no entry-point symbol — the
//! script *is* the entry point.
//!
//! Backends consume an `IRScript` directly:
//!
//! - The interpreter (`expo-alpha-ir-eval`) drives `script.blocks`
//!   through the same instruction walker it uses for an
//!   `IRFunction.blocks`, looking up callees in `script.packages`.
//! - The LLVM backend (`expo-alpha-ir-llvm`) emits `script.blocks`
//!   as the body of a host-runtime `main` function and walks
//!   `script.packages` for non-entry function declarations.
//!
//! The shape mirrors a single function's body without leaking an
//! [`IRFunction`] (which carries a name, parameters, and the
//! "user-declared" semantics that scripts deliberately don't have).

use expo_alpha_typecheck::CheckedProgram;

use crate::enum_decl::IREnumDecl;
use crate::error::LowerError;
use crate::function::{IRBasicBlock, IRFunction};
use crate::generics;
use crate::lower::{LowerOutput, lower_body_to_blocks, lower_package};
use crate::package::IRPackage;
use crate::seal;
use crate::struct_decl::IRStructDecl;
use crate::types::IRType;

/// Sealed output of [`lower_script`]'s success path.
///
/// `blocks` is the implicit function's body — the top-level
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
/// `return_type` is the static type of the script's trailing
/// expression value (or `IRType::Unit` for an empty / non-expression
/// trailing statement). Backends consume this directly to size the
/// `main` return slot and the `Return` terminator's value width.
#[derive(Debug, Clone)]
pub struct IRScript {
    pub blocks: Vec<IRBasicBlock>,
    pub packages: Vec<IRPackage>,
    pub return_type: IRType,
}

impl IRScript {
    /// Lookup a helper function across every package by its mangled
    /// symbol. Mirrors [`crate::IRProgram::function`] so the
    /// interpreter and LLVM backend can drive a single shared
    /// instruction walker over either IR shape — only the
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
}

/// Run every sub-pass in the alpha lowering phase against a
/// script-mode [`CheckedProgram`].
///
/// Pure with respect to its input. Per-function feature gaps surface
/// as [`Diagnostic`]s and the offending function (or the script body
/// itself) is dropped from the result; seal violations panic per
/// northstar.
///
/// Pipeline contract: at most one file across all `checked.packages`
/// may carry a populated `body`. Zero (an items-only script — every
/// REPL session before the user types a top-level expression looks
/// like this) is treated as an implicit `Unit`-returning empty body.
/// More than one is a driver invariant violation and panics with a
/// seal-style message — the driver dispatches script-mode lowering
/// on a single source file.
///
/// Sub-pass order:
///
/// 1. `lower_package` per package (same path [`crate::lower_program`]
///    uses) so any `fn helper -> Int / 1 / end` decls in the script
///    source are available to call.
/// 2. Locate the unique file with `body.is_some()` across the input
///    and lower its statements through the shared
///    [`lower_body_to_blocks`] helper.
/// 3. Bail with `Err(LowerError::Diagnostics)` if any
///    feature-gap diagnostic surfaced (per-function fail-fast).
/// 4. Run `seal::seal_script` on the assembled script. Panics on
///    violation per the seal contract.
pub fn lower_script(checked: &CheckedProgram) -> Result<IRScript, LowerError> {
    let mut output = LowerOutput::default();
    let mut packages: Vec<IRPackage> = Vec::with_capacity(checked.packages.len());
    for pkg in &checked.packages {
        packages.push(lower_package(pkg, &checked.registry, &mut output));
    }

    let body = locate_script_body(checked);

    let lowered = lower_body_to_blocks(body, &checked.registry, &mut output);

    if !output.diagnostics.is_empty() {
        return Err(LowerError::Diagnostics(output.diagnostics));
    }

    let (blocks, return_type) = lowered.unwrap_or_else(|()| {
        panic!(
            "alpha IR lower_script: body lowering returned Err(()) without pushing diagnostics — \
             lower_body_to_blocks contract violation",
        )
    });

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

    let script = IRScript {
        blocks,
        packages,
        return_type,
    };
    seal::seal_script(&script);
    Ok(script)
}

/// Find the populated `File.body` in `checked`, or fall back to an
/// empty slice when no file carries one (a script source that's
/// items-only, e.g. a REPL session before the user types a
/// trailing expression). Panics if more than one file carries a
/// body — the driver must dispatch script-mode lowering on a single
/// source file.
fn locate_script_body(checked: &CheckedProgram) -> &[expo_ast::ast::Statement] {
    let mut body: Option<&[expo_ast::ast::Statement]> = None;
    for pkg in &checked.packages {
        for file in &pkg.files {
            if let Some(stmts) = file.body.as_deref() {
                if body.is_some() {
                    panic!(
                        "alpha IR lower_script: more than one file carries `File.body` — \
                         the driver must dispatch script-mode lowering on a single source",
                    );
                }
                body = Some(stmts);
            }
        }
    }
    body.unwrap_or(&[])
}
