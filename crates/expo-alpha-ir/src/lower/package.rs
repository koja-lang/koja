//! Package- and function-shaped lowering entry points. Walks one
//! sealed [`CheckedPackage`] into an [`IRPackage`] fragment, calling
//! into [`super::body`] for the per-function body work.
//!
//! Also owns the [`GlobalRegistry`] adapters every helper needs:
//! [`lookup_signature`] (registry → lifted [`FunctionSignature`]) and
//! [`resolved_type_to_ir_type`] (typecheck `ResolvedType` → `IRType`).
//! Keeping them here lets `body.rs` / `expr.rs` import a stable
//! seam without re-coding the registry shape.

use expo_alpha_typecheck::{CheckedPackage, FunctionSignature, GlobalKind, GlobalRegistry};
use expo_ast::ast::{Diagnostic, Function, Item, Param};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};

use crate::function::{IRFunction, IRFunctionParam, IRSymbol};
use crate::package::IRPackage;
use crate::types::IRType;

use super::body::{finalize_open_flow, lower_body};
use super::ctx::FnLowerCtx;

use std::collections::BTreeMap;

pub(crate) fn lower_package(
    pkg: &CheckedPackage,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> IRPackage {
    let mut functions = BTreeMap::new();
    for file in &pkg.files {
        for item in &file.items {
            if let Item::Function(function) = item
                && let Some(lowered) = lower_function(function, &pkg.package, registry, diagnostics)
            {
                functions.insert(lowered.symbol.clone(), lowered);
            }
        }
    }
    IRPackage {
        functions,
        package: pkg.package.clone(),
    }
}

/// Lower a single [`Function`] or return `None` if any feature-gap
/// diagnostic surfaced while lowering it. The function is simply
/// omitted from the package in that case; `lower_program` will turn
/// the accumulated diagnostics into a [`crate::LowerError::Diagnostics`]
/// before seal runs.
fn lower_function(
    function: &Function,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<IRFunction> {
    let identifier = Identifier::new(package, vec![function.name.clone()]);
    let Some(body) = function.body.as_ref() else {
        diagnostics.push(Diagnostic::error(
            format!(
                "alpha IR does not yet lower extern fn `{}` (no body to lower)",
                function.name,
            ),
            function.span,
        ));
        return None;
    };

    let signature = lookup_signature(registry, &identifier);
    let return_type = resolved_type_to_ir_type(&signature.return_type, registry);

    let mut ctx = FnLowerCtx::new();
    let entry = ctx.fresh_block("entry");

    // Allocate one `ValueId` per regular parameter in declaration
    // order, paired with its IRType pulled from the lifted function
    // signature on the registry. Pre-allocation ensures every param
    // id is strictly less than any body-produced id — body lowering
    // stays naturally topological on the sealed AST. `self` receivers
    // are a feature gap, not a compiler bug: record a diagnostic and
    // bail on this function.
    let mut params = Vec::with_capacity(function.params.len());
    let mut signature_index = 0;
    for param in &function.params {
        match param {
            Param::Regular { .. } => {
                let resolved = &signature.params[signature_index].ty;
                let ty = resolved_type_to_ir_type(resolved, registry);
                signature_index += 1;
                let id = ctx.fresh_value(ty.clone());
                params.push(IRFunctionParam { id, ty });
            }
            Param::Self_ { span, .. } => {
                diagnostics.push(Diagnostic::error(
                    format!("alpha IR does not yet lower `self` receivers (on `{identifier}`)",),
                    *span,
                ));
                return None;
            }
        }
    }

    let flow = lower_body(body, &mut ctx, entry, registry, diagnostics).ok()?;
    finalize_open_flow(&mut ctx, flow);

    let blocks = ctx.into_blocks();
    Some(IRFunction {
        blocks,
        params,
        return_type,
        symbol: IRSymbol::from_identifier(&identifier),
    })
}

/// Lookup the lifted [`FunctionSignature`] for `identifier` in the
/// registry. The seal contract guarantees a registered function has
/// a `Some(_)` signature stamped by `lift_signatures`, so a miss or
/// `None` here is a compiler bug, not a feature gap.
pub(super) fn lookup_signature<'a>(
    registry: &'a GlobalRegistry,
    identifier: &Identifier,
) -> &'a FunctionSignature {
    let entry = registry.lookup(identifier).unwrap_or_else(|| {
        panic!(
            "alpha IR lower: function `{identifier}` not in registry — \
             collect/seal invariant violation",
        );
    });
    match &entry.1.kind {
        GlobalKind::Function(Some(sig)) => sig,
        other => panic!(
            "alpha IR lower: function `{identifier}` has no lifted signature \
             ({}) — lift_signatures invariant violation",
            other.label(),
        ),
    }
}

/// Translate a typecheck-resolved [`ResolvedType`] to an [`IRType`].
///
/// Today the alpha registry's stdlib stubs only cover the scalars
/// alpha typecheck synthesizes from literals (`Int`, `Bool`, `Unit`,
/// `Float`, `String`). Anything else — width-explicit ints, user
/// structs, polymorphic containers — is a feature gap and panics with
/// a "not yet translatable" message. As stdlib stubs grow this match
/// grows in lockstep.
pub(super) fn resolved_type_to_ir_type(ty: &ResolvedType, registry: &GlobalRegistry) -> IRType {
    let Resolution::Global(id) = ty.resolution else {
        panic!(
            "alpha IR lower: ResolvedType has Unresolved resolution after typecheck seal — \
             compiler bug",
        );
    };
    let entry = registry.get(id).unwrap_or_else(|| {
        panic!("alpha IR lower: ResolvedType id {id} missing from registry — seal violation",)
    });
    if !entry.identifier.is_in_package("Global") {
        panic!(
            "alpha IR lower: cannot translate non-`Global` type `{}` to IRType yet",
            entry.identifier,
        );
    }
    match entry.identifier.last() {
        "Bool" => IRType::Bool,
        "Float" => IRType::Float64,
        "Int" => IRType::Int64,
        "String" => IRType::String,
        "Unit" => IRType::Unit,
        other => panic!(
            "alpha IR lower: cannot translate `Global.{other}` to IRType yet \
             (`Float32` / `Float64` annotations and width-explicit ints land \
              in follow-up slices)",
        ),
    }
}
