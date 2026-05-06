//! Package- and function-shaped lowering entry points. Walks one
//! sealed [`CheckedPackage`] into an [`IRPackage`] fragment, calling
//! into [`super::body`] for the per-function body work.
//!
//! Also owns the [`GlobalRegistry`] adapters every helper needs:
//! [`lookup_signature`] (registry → lifted [`FunctionSignature`]) and
//! [`resolved_type_to_ir_type`] (typecheck `ResolvedType` → `IRType`).
//! Keeping them here lets `body.rs` / `expr.rs` import a stable
//! seam without re-coding the registry shape.
//!
//! Static methods declared inline in `struct ... end` or in
//! `impl Type ... end` blocks lower through the same
//! [`lower_function_with_identifier`] helper as top-level functions —
//! the only difference is the [`Identifier`] (and therefore the
//! [`IRSymbol`]) the caller picks. Both forms land in the package's
//! shared `functions: BTreeMap<IRSymbol, IRFunction>` map, so
//! downstream consumers can't tell which surface form declared them.

use expo_alpha_typecheck::{CheckedPackage, FunctionSignature, GlobalKind, GlobalRegistry};
use expo_ast::ast::{
    Diagnostic, Function, ImplBlock, ImplMember, Item, Param, TypeExpr, is_intrinsic,
};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};

use crate::function::{FunctionKind, IRFunction, IRFunctionParam, IRSymbol};
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::types::IRType;

use super::body::{finalize_open_flow, lower_body};
use super::ctx::FnLowerCtx;
use super::structs::lower_struct_decl;

use std::collections::BTreeMap;

pub(crate) fn lower_package(
    pkg: &CheckedPackage,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> IRPackage {
    let mut functions: BTreeMap<IRSymbol, IRFunction> = BTreeMap::new();
    let mut structs: BTreeMap<IRSymbol, IRStructDecl> = BTreeMap::new();
    for file in &pkg.files {
        for item in &file.items {
            match item {
                Item::Function(function) => {
                    let identifier = Identifier::new(&pkg.package, vec![function.name.clone()]);
                    if let Some(lowered) =
                        lower_function_with_identifier(function, identifier, registry, diagnostics)
                    {
                        functions.insert(lowered.symbol.clone(), lowered);
                    }
                }
                Item::Struct(decl) => {
                    if let Some(lowered) =
                        lower_struct_decl(decl, &pkg.package, registry, diagnostics)
                    {
                        structs.insert(lowered.symbol.clone(), lowered);
                    }
                    for function in &decl.functions {
                        let identifier = Identifier::new(
                            &pkg.package,
                            vec![decl.name.clone(), function.name.clone()],
                        );
                        if let Some(lowered) = lower_function_with_identifier(
                            function,
                            identifier,
                            registry,
                            diagnostics,
                        ) {
                            functions.insert(lowered.symbol.clone(), lowered);
                        }
                    }
                }
                Item::Impl(impl_block) => {
                    lower_impl(
                        impl_block,
                        &pkg.package,
                        registry,
                        diagnostics,
                        &mut functions,
                    );
                }
                _ => {}
            }
        }
    }
    IRPackage {
        functions,
        package: pkg.package.clone(),
        structs,
    }
}

/// Lower every static method declared in an `impl Type ... end`
/// block. Trait impls, generic targets, and `TypeAlias` members are
/// already diagnosed by typecheck collect / lift; reaching IR with
/// any of them would be a seal violation, so we silently skip.
fn lower_impl(
    impl_block: &ImplBlock,
    package: &str,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
    functions: &mut BTreeMap<IRSymbol, IRFunction>,
) {
    if impl_block.trait_expr.is_some() {
        return;
    }
    let Some(target_name) = impl_target_name(&impl_block.target) else {
        return;
    };
    for member in &impl_block.members {
        let ImplMember::Function(function) = member else {
            continue;
        };
        let identifier = Identifier::new(
            package,
            vec![target_name.to_string(), function.name.clone()],
        );
        if let Some(lowered) =
            lower_function_with_identifier(function, identifier, registry, diagnostics)
        {
            functions.insert(lowered.symbol.clone(), lowered);
        }
    }
}

fn impl_target_name(target: &TypeExpr) -> Option<&str> {
    match target {
        TypeExpr::Named { path, .. } if path.len() == 1 => Some(path[0].as_str()),
        _ => None,
    }
}

/// Lower a single [`Function`] under `identifier` (top-level, inline
/// struct method, or impl-block method — all three flow through
/// here). Returns `None` if any feature-gap diagnostic surfaced; the
/// function is omitted from the package in that case.
///
/// Three shapes flow through here, distinguished by annotation +
/// body presence (mutually exclusive at the source level; mixing
/// markers diagnoses):
///
/// - `@intrinsic fn name(...)` (no body) lowers to
///   [`FunctionKind::Intrinsic`] with empty `blocks`. The backend
///   looks the body up by mangled symbol in its `intrinsics/`
///   dispatch table.
/// - `@extern "C" fn name(...)` (no body) is a planned follow-up;
///   today it surfaces a feature-gap diagnostic.
/// - Regular `fn name(...)` (body present) lowers to
///   [`FunctionKind::Regular`] with at least one basic block.
pub(super) fn lower_function_with_identifier(
    function: &Function,
    identifier: Identifier,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<IRFunction> {
    let signature = lookup_signature(registry, &identifier)?;
    let return_type = resolved_type_to_ir_type(&signature.return_type, registry);
    let intrinsic = is_intrinsic(&function.annotations);

    if intrinsic && function.body.is_some() {
        diagnostics.push(Diagnostic::error(
            format!("`@intrinsic` and a function body are mutually exclusive (on `{identifier}`)",),
            function.span,
        ));
        return None;
    }

    let mut ctx = FnLowerCtx::new();
    let params = lower_params(
        function,
        &identifier,
        signature,
        registry,
        diagnostics,
        &mut ctx,
    )?;

    if intrinsic {
        return Some(IRFunction {
            blocks: Vec::new(),
            kind: FunctionKind::Intrinsic,
            params,
            return_type,
            symbol: IRSymbol::from_identifier(&identifier),
        });
    }

    let Some(body) = function.body.as_ref() else {
        diagnostics.push(Diagnostic::error(
            format!("alpha IR does not yet lower extern fn `{identifier}` (no body to lower)",),
            function.span,
        ));
        return None;
    };

    let entry = ctx.fresh_block("entry");
    let flow = lower_body(body, &mut ctx, entry, registry, diagnostics).ok()?;
    finalize_open_flow(&mut ctx, flow);

    let blocks = ctx.into_blocks();
    Some(IRFunction {
        blocks,
        kind: FunctionKind::Regular,
        params,
        return_type,
        symbol: IRSymbol::from_identifier(&identifier),
    })
}

/// Allocate one [`ValueId`] per regular parameter in declaration
/// order, paired with its IRType pulled from the lifted function
/// signature on the registry. Pre-allocation ensures every param id
/// is strictly less than any body-produced id — body lowering stays
/// naturally topological on the sealed AST. `self` receivers are a
/// feature gap, not a compiler bug: record a diagnostic and bail on
/// this function.
///
/// [`ValueId`]: crate::types::ValueId
fn lower_params(
    function: &Function,
    identifier: &Identifier,
    signature: &FunctionSignature,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
    ctx: &mut FnLowerCtx,
) -> Option<Vec<IRFunctionParam>> {
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
    Some(params)
}

/// Lookup the lifted [`FunctionSignature`] for `identifier` in the
/// registry. Returns `None` if the registry doesn't carry an entry —
/// typecheck collect / lift may have rejected this function (e.g.
/// `self` receiver, impl on unknown type), in which case IR silently
/// skips it. A registered entry without a `Some(_)` signature is a
/// compiler bug, not a feature gap.
pub(super) fn lookup_signature<'a>(
    registry: &'a GlobalRegistry,
    identifier: &Identifier,
) -> Option<&'a FunctionSignature> {
    let (_, entry) = registry.lookup(identifier)?;
    match &entry.kind {
        GlobalKind::Function(Some(sig)) => Some(sig),
        other => panic!(
            "alpha IR lower: function `{identifier}` has no lifted signature \
             ({}) — lift_signatures invariant violation",
            other.label(),
        ),
    }
}

/// Translate a typecheck-resolved [`ResolvedType`] to an [`IRType`].
///
/// Two shapes today: stdlib primitives (`Bool` / `Float` / `Int` /
/// `String` / `Unit`) under the `Global` package map to their
/// matching scalar [`IRType`]; user-declared structs (any package)
/// map to [`IRType::Struct`] keyed by the canonical
/// [`IRSymbol::from_identifier`] for the entry. Width-explicit ints
/// and polymorphic containers stay feature gaps.
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
    if entry.identifier.is_in_package("Global") {
        return match entry.identifier.last() {
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
        };
    }
    match &entry.kind {
        GlobalKind::Struct(_) => IRType::Struct(IRSymbol::from_identifier(&entry.identifier)),
        other => panic!(
            "alpha IR lower: cannot translate `{}` ({}) to IRType yet",
            entry.identifier,
            other.label(),
        ),
    }
}
