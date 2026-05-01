//! Whole-program monomorphization closure pass.
//!
//! Sits between per-function lowering and the [`crate::elaborate`] seam:
//!
//! 1. Lower (per-function): produces an [`crate::IRProgram`] populated
//!    with non-generic decls; generic call/construction sites still
//!    refer to as-yet-unregistered mangled names.
//! 2. **Closure pass (this module).** Walks every function body's AST
//!    looking for generic instantiations and registers them with
//!    [`crate::IRProgram`] via the existing planners
//!    ([`crate::lower::monomorphize`]).
//! 3. Elaborate: post-mono coercion sub-passes (protocol rewrites,
//!    phi coercion, numeric staging). Assumes the closure pass has
//!    completed.
//! 4. Backends consume a sealed [`crate::IRProgram`].
//!
//! ## Slice coverage
//!
//! - Slice 1: struct + enum instantiations ([`types`]).
//! - Slice 2: free-function instantiations ([`functions`]).
//! - Slice 3: user impl-method instantiations ([`methods`]).
//!
//! Each slice plugs another sub-walk into [`closure_program`]. The
//! per-walk traversal scaffolding (descend into block bodies,
//! conditional arms, etc.) lives in [`visit`] and is shared.

mod functions;
mod methods;
mod types;
mod visit;

use std::collections::HashMap;

use expo_ast::ast::Function;
use expo_typecheck::context::TypeContext;

use crate::TypeLayouts;
use crate::program::IRProgram;

/// A safety bound on outer-loop iterations. Each sub-walk is
/// idempotent so the loop terminates as soon as a pass adds no new
/// decls; the cap defends against a future change introducing a
/// non-idempotent path.
const MAX_OUTER_ITERATIONS: usize = 1024;

/// Run the whole-program monomorphization closure pass over `program`,
/// registering every reachable generic instantiation through the
/// [`crate::lower::monomorphize`] planners.
///
/// `generic_fn_asts` is the codegen-side cache of generic free
/// function ASTs (keyed by source name); the planner consults it to
/// build a monomorphized [`crate::IRFunction`]'s body. `type_layouts`
/// is reserved for future slices that need to consult the existing
/// type-layout cache; the slice 1/2 sub-walks do not yet read from it.
///
/// Sub-walks run in a fixpoint loop: a freshly monomorphized function
/// body may reference more generic types or call more generic
/// functions, so we re-run until both walks converge.
pub fn closure_program(
    program: &mut IRProgram,
    type_ctx: &TypeContext,
    _type_layouts: &TypeLayouts,
    generic_fn_asts: &HashMap<String, Function>,
) -> Result<(), String> {
    for _ in 0..MAX_OUTER_ITERATIONS {
        let before_structs = program.structs.len();
        let before_enums = program.enums.len();
        let before_functions = program.functions.len();
        types::run(program, type_ctx)?;
        functions::run(program, type_ctx, generic_fn_asts)?;
        methods::run(program, type_ctx)?;
        if program.structs.len() == before_structs
            && program.enums.len() == before_enums
            && program.functions.len() == before_functions
        {
            return Ok(());
        }
    }
    Err(format!(
        "closure pass exceeded {MAX_OUTER_ITERATIONS} outer iterations; \
         suspect a non-idempotent sub-walk or runaway recursive instantiation"
    ))
}
