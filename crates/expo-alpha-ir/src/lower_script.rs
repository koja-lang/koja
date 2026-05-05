//! Lower-script sub-pass: translate a sealed
//! [`expo_alpha_typecheck::CheckedProgram`] whose source was parsed in
//! `ParseMode::Script` into an [`IRScript`].
//!
//! Pure with respect to its input. Per-function feature gaps surface
//! as [`Diagnostic`]s and the offending function (or the script body
//! itself) is dropped from the result; seal violations panic per
//! northstar.
//!
//! Pipeline contract: at most one file across all `checked.packages`
//! may carry a populated `body`. Zero (an items-only script — every
//! REPL session before the user types a top-level expression looks
//! like this) is treated as an implicit `Unit`-returning empty body.
//! More than one is a driver invariant violation and panics with a
//! seal-style message — the driver dispatches script-mode lowering
//! on a single source file.

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::Diagnostic;

use crate::lower_package::{BlockBuilder, lower_body_to_blocks, lower_package};
use crate::package::IRPackage;
use crate::program::LowerError;
use crate::script::IRScript;
use crate::seal;

/// Run the lower-script sub-pass.
///
/// 1. `lower_package` per package (same path `lower_program` uses)
///    so any `fn helper -> Int / 1 / end` decls in the script source
///    are available to call.
/// 2. Locate the unique file with `body.is_some()` across the input
///    and lower its statements through the shared
///    [`lower_body_to_blocks`] helper.
/// 3. Bail with `Err(LowerError::Diagnostics)` if any
///    feature-gap diagnostic surfaced (per-function fail-fast).
/// 4. Run [`seal::seal_script`] on the assembled script. Panics on
///    violation per the seal contract.
pub fn lower_script(checked: &CheckedProgram) -> Result<IRScript, LowerError> {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut packages: Vec<IRPackage> = Vec::with_capacity(checked.packages.len());
    for pkg in &checked.packages {
        packages.push(lower_package(pkg, &checked.registry, &mut diagnostics));
    }

    let body = locate_script_body(checked);

    let mut builder = BlockBuilder::default();
    let lowered = lower_body_to_blocks(body, &mut builder, &checked.registry, &mut diagnostics);

    if !diagnostics.is_empty() {
        return Err(LowerError::Diagnostics(diagnostics));
    }

    let (blocks, return_type) = lowered.unwrap_or_else(|()| {
        panic!(
            "alpha IR lower_script: body lowering returned Err(()) without pushing diagnostics — \
             lower_body_to_blocks contract violation",
        )
    });

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
