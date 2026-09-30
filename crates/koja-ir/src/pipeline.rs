//! The lowering stages [`crate::lower_program`] and
//! [`crate::lower_script`] share. Each entry stages its own shape
//! between [`lower_packages`] and [`instantiate`] (a `Process` entry
//! wrapper, or an inline script body), then runs [`rewrite`] and
//! [`built_constant_order`] before it seals.

use std::collections::{BTreeMap, BTreeSet};

use koja_typecheck::{CheckedProgram, GlobalRegistry};

use crate::built_order;
use crate::cycle::break_type_cycles;
use crate::elaborate::elaborate;
use crate::error::LowerError;
use crate::function::{FunctionKind, IRBasicBlock, IRSymbol};
use crate::generics;
use crate::lower::{LowerOutput, lower_package};
use crate::merge;
use crate::package::IRPackage;
use crate::tail_calls::rewrite_tail_calls;
use crate::union_decl::discover_unions;
use crate::yield_checks::insert_yield_checks;

/// The order backends run [`crate::IRConstantValue::Built`] inits in.
/// A startup cycle returns its diagnostics as a [`LowerError`].
pub(crate) fn built_constant_order(
    packages: &[IRPackage],
    registry: &GlobalRegistry,
) -> Result<Vec<IRSymbol>, LowerError> {
    built_order::built_constant_order(packages, |symbol| {
        built_order::constant_span(symbol, registry)
    })
    .map_err(LowerError::Diagnostics)
}

/// Surface the diagnostics lowering has accumulated so far as a
/// [`LowerError`], or `Ok` when there are none.
pub(crate) fn check_diagnostics(output: &mut LowerOutput) -> Result<(), LowerError> {
    if output.diagnostics.is_empty() {
        return Ok(());
    }
    Err(LowerError::Diagnostics(std::mem::take(
        &mut output.diagnostics,
    )))
}

/// Walk every `@extern "C"` function across `packages` and collect a
/// deduped, sorted list of `link_lib` names. Used at lower time so
/// backends and cache layers don't re-walk the IR. Functions without
/// a `link_lib` (bare `@extern "C"` with no `@link`) contribute
/// nothing. The C symbol is still resolved via the normal libc /
/// runtime search path at link time.
pub(crate) fn collect_link_libraries<'a, I>(packages: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a IRPackage>,
{
    let mut libs = BTreeSet::new();
    for pkg in packages {
        for function in pkg.functions.values() {
            if let FunctionKind::Extern(attrs) = &function.kind
                && let Some(lib) = &attrs.link_lib
            {
                libs.insert(lib.clone());
            }
        }
    }
    libs.into_iter().collect()
}

/// Empty `Global` IRPackage seeded so `generics::monomorphize` has a
/// place to land stdlib stub instantiations (today only `Option<T>`).
fn empty_global_stdlib_package() -> IRPackage {
    IRPackage {
        constants: BTreeMap::new(),
        enums: BTreeMap::new(),
        functions: BTreeMap::new(),
        package: "Global".to_string(),
        structs: BTreeMap::new(),
        unions: BTreeMap::new(),
    }
}

/// Run the queued generic instantiations against `packages`. The
/// diagnostics they raise return as a [`LowerError`].
pub(crate) fn instantiate(
    packages: &mut [IRPackage],
    checked: &CheckedProgram,
    output: &mut LowerOutput,
) -> Result<(), LowerError> {
    let initial = std::mem::take(&mut output.instantiations);
    generics::instantiate(
        initial,
        &checked.registry,
        &checked.packages,
        packages,
        output,
    );
    check_diagnostics(output)
}

/// Lower every checked package behind a seeded `Global` package and
/// coalesce the fragments by package name.
pub(crate) fn lower_packages(checked: &CheckedProgram, output: &mut LowerOutput) -> Vec<IRPackage> {
    let mut packages = Vec::with_capacity(checked.packages.len() + 1);
    packages.push(empty_global_stdlib_package());
    for pkg in &checked.packages {
        packages.push(lower_package(pkg, &checked.registry, output));
    }
    merge::coalesce(packages)
}

/// The post-instantiation rewrites. Union discovery runs first, then
/// type cycle breaking, tail-call rewriting, yield insertion, and
/// elaboration. `body` is a script's inline top-level body. Programs
/// pass an empty one.
pub(crate) fn rewrite(packages: &mut [IRPackage], body: &mut [IRBasicBlock]) {
    discover_unions(packages, body);
    break_type_cycles(packages);
    rewrite_tail_calls(packages);
    insert_yield_checks(packages, body);
    elaborate(packages, body);
}
