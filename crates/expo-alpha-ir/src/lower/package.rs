//! Package- and function-shaped lowering entry points. Walks a
//! sealed [`CheckedPackage`] into an [`IRPackage`] fragment, delegating
//! per-function body work to [`super::body`]. Also owns the
//! [`GlobalRegistry`] adapters ([`function_signature`],
//! [`resolved_type_to_ir_type`]) so siblings import a stable seam.
//!
//! Top-level / inline-struct / `impl`-block functions all flow
//! through [`lower_function_with_identifier`] — only the
//! [`Identifier`] differs.

use expo_alpha_typecheck::{CheckedPackage, FunctionSignature, GlobalKind, GlobalRegistry};
use expo_ast::ast::{
    Diagnostic, Function, ImplBlock, ImplMember, Item, Param, TypeExpr, is_intrinsic,
};
use expo_ast::identifier::{GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType};

use crate::enum_decl::IREnumDecl;
use crate::function::{FunctionKind, IRFunction, IRFunctionParam, IRInstruction, IRSymbol};
use crate::generics::Instantiation;
use crate::local::IRLocalId;
use crate::mangling::mangled_type_name;
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::types::IRType;

use super::body::{finalize_open_flow, lower_body};
use super::ctx::{FnLowerCtx, LowerOutput};
use super::enums::lower_enum_decl;
use super::structs::lower_struct_decl;

use std::collections::BTreeMap;

/// Lower one [`CheckedPackage`] into an [`IRPackage`] fragment.
/// Generic struct / enum decls are skipped here — they live in the
/// typecheck registry and only become concrete decls when
/// [`crate::generics::instantiate`] specializes them. Concrete
/// instantiations encountered while lowering construction sites,
/// field types, or function signatures append to
/// `output.instantiations` for the driver to monomorphize;
/// feature-gap diagnostics push to `output.diagnostics` and the
/// offending decl is dropped.
pub(crate) fn lower_package(
    pkg: &CheckedPackage,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> IRPackage {
    let mut enums: BTreeMap<IRSymbol, IREnumDecl> = BTreeMap::new();
    let mut functions: BTreeMap<IRSymbol, IRFunction> = BTreeMap::new();
    let mut structs: BTreeMap<IRSymbol, IRStructDecl> = BTreeMap::new();
    for file in &pkg.files {
        for item in &file.items {
            match item {
                Item::Enum(decl) => {
                    if let Some(lowered) = lower_enum_decl(decl, &pkg.package, registry, output) {
                        enums.insert(lowered.symbol.clone(), lowered);
                    }
                    for function in &decl.functions {
                        let identifier = Identifier::new(
                            &pkg.package,
                            vec![decl.name.clone(), function.name.clone()],
                        );
                        if let Some(lowered) =
                            lower_function_with_identifier(function, identifier, registry, output)
                        {
                            functions.insert(lowered.symbol.clone(), lowered);
                        }
                    }
                }
                Item::Function(function) => {
                    let identifier = Identifier::new(&pkg.package, vec![function.name.clone()]);
                    if let Some(lowered) =
                        lower_function_with_identifier(function, identifier, registry, output)
                    {
                        functions.insert(lowered.symbol.clone(), lowered);
                    }
                }
                Item::Struct(decl) => {
                    if let Some(lowered) = lower_struct_decl(decl, &pkg.package, registry, output) {
                        structs.insert(lowered.symbol.clone(), lowered);
                    }
                    for function in &decl.functions {
                        let identifier = Identifier::new(
                            &pkg.package,
                            vec![decl.name.clone(), function.name.clone()],
                        );
                        if let Some(lowered) =
                            lower_function_with_identifier(function, identifier, registry, output)
                        {
                            functions.insert(lowered.symbol.clone(), lowered);
                        }
                    }
                }
                Item::Impl(impl_block) => {
                    lower_impl(impl_block, &pkg.package, registry, output, &mut functions);
                }
                _ => {}
            }
        }
    }
    IRPackage {
        enums,
        functions,
        package: pkg.package.clone(),
        structs,
    }
}

/// Lower methods declared in an `impl Type ... end` or
/// `impl Trait for Type ... end` block. Unsupported targets already
/// errored upstream; IR silently skips them. Trait-impl members
/// (including synthesized default-method bodies) lower the same as
/// inherent ones — both register at `Package.Type.method` and the
/// IR doesn't model the trait link.
fn lower_impl(
    impl_block: &ImplBlock,
    package: &str,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
    functions: &mut BTreeMap<IRSymbol, IRFunction>,
) {
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
            lower_function_with_identifier(function, identifier, registry, output)
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

/// Lower one [`Function`] under `identifier`. `@intrinsic`-annotated
/// functions become [`FunctionKind::Intrinsic`] with empty blocks
/// (backends synthesize bodies from a mangled-symbol table); regular
/// functions become [`FunctionKind::Regular`] with at least one
/// basic block. Returns `None` (with a diagnostic) on feature gaps.
pub(super) fn lower_function_with_identifier(
    function: &Function,
    identifier: Identifier,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Option<IRFunction> {
    let signature = function_signature(registry, &identifier)?;
    let return_type =
        resolved_type_to_ir_type(&signature.return_type, registry, &mut output.instantiations);
    let intrinsic = is_intrinsic(&function.annotations);

    if intrinsic && function.body.is_some() {
        output.diagnostics.push(Diagnostic::error(
            format!("`@intrinsic` and a function body are mutually exclusive (on `{identifier}`)",),
            function.span,
        ));
        return None;
    }

    let mut ctx = FnLowerCtx::new();

    if intrinsic {
        let params = lower_intrinsic_params(function, signature, registry, output, &mut ctx)?;
        return Some(IRFunction {
            blocks: Vec::new(),
            kind: FunctionKind::Intrinsic,
            params,
            return_type,
            symbol: IRSymbol::from_identifier(&identifier),
        });
    }

    let Some(body) = function.body.as_ref() else {
        output.diagnostics.push(Diagnostic::error(
            format!("alpha IR does not yet lower extern fn `{identifier}` (no body to lower)",),
            function.span,
        ));
        return None;
    };

    let entry = ctx.fresh_block("entry");
    let params = lower_params(function, &identifier, signature, registry, output, &mut ctx)?;

    let flow = lower_body(body, &mut ctx, entry, registry, output).ok()?;
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

/// Mint a [`ValueId`](crate::types::ValueId) per parameter (in
/// declaration order, `self` included) and promote each into a local
/// slot via `LocalDecl` + `LocalWrite` appended to the entry block.
/// `self` is treated as a regular param here: typecheck stamps
/// `local_id` on every param shape, and `ExprKind::Self_` references
/// read through the same `LocalRead` path body locals use.
fn lower_params(
    function: &Function,
    identifier: &Identifier,
    signature: &FunctionSignature,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
    ctx: &mut FnLowerCtx,
) -> Option<Vec<IRFunctionParam>> {
    let mut params = Vec::with_capacity(function.params.len());
    for (index, param) in function.params.iter().enumerate() {
        let local_id = param_local_id(param).unwrap_or_else(|| {
            panic!(
                "alpha IR lower: `{identifier}` parameter #{index} carries no `LocalId` — \
                 typecheck resolve must stamp one for every param before lower runs",
            )
        });
        let resolved = &signature.params[index].ty;
        let ty = resolved_type_to_ir_type(resolved, registry, &mut output.instantiations);
        let id = ctx.fresh_value(ty.clone());
        let ir_local = IRLocalId::from_local_id(local_id);
        let entry = ctx.entry_block();
        ctx.cfg.append(
            entry,
            IRInstruction::LocalDecl {
                local: ir_local,
                ty: ty.clone(),
            },
        );
        ctx.cfg.append(
            entry,
            IRInstruction::LocalWrite {
                local: ir_local,
                value: id,
            },
        );
        ctx.mark_local_declared(ir_local);
        params.push(IRFunctionParam {
            id,
            local_id: ir_local,
            ty,
        });
    }
    Some(params)
}

/// Mint params for an `@intrinsic` function. No entry block, no
/// promotion: backends synthesize the body and never walk the
/// (empty) blocks.
fn lower_intrinsic_params(
    function: &Function,
    signature: &FunctionSignature,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
    ctx: &mut FnLowerCtx,
) -> Option<Vec<IRFunctionParam>> {
    let mut params = Vec::with_capacity(function.params.len());
    for (index, param) in function.params.iter().enumerate() {
        let local_id = param_local_id(param).unwrap_or_else(|| {
            panic!(
                "alpha IR lower: intrinsic parameter #{index} carries no `LocalId` — \
                 typecheck resolve invariant violation",
            )
        });
        let resolved = &signature.params[index].ty;
        let ty = resolved_type_to_ir_type(resolved, registry, &mut output.instantiations);
        let id = ctx.fresh_value(ty.clone());
        params.push(IRFunctionParam {
            id,
            local_id: IRLocalId::from_local_id(local_id),
            ty,
        });
    }
    Some(params)
}

/// Pluck the AST `LocalId` off a param. Resolve stamps every param,
/// so `None` is an invariant violation, not a feature gap.
fn param_local_id(param: &Param) -> Option<LocalId> {
    match param {
        Param::Regular { local_id, .. } | Param::Self_ { local_id, .. } => *local_id,
    }
}

/// Lookup the lifted [`FunctionSignature`] for `identifier`.
/// Returns `None` when collect / lift rejected the function (IR
/// silently skips); a registered entry without a signature panics
/// as an invariant violation.
pub(super) fn function_signature<'a>(
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

/// Translate a typecheck [`ResolvedType`] to a concrete [`IRType`].
/// Stdlib `Global.{Bool,Float,Int,String,Unit}` map to scalar
/// [`IRType`]s; user structs / enums map to [`IRType::Struct`] /
/// [`IRType::Enum`] — with concrete `type_args` folded into the
/// symbol via [`mangled_type_name`]. Every non-empty-args
/// translation also pushes an [`Instantiation`] (keyed at the
/// template's [`GlobalRegistryId`]) for the
/// [`crate::generics::instantiate`] driver to specialize.
///
/// Panics on `Resolution::TypeParam` — by the time IR lowers a
/// type, every `Param` should have been substituted by the caller
/// (typecheck for resolved expressions; the monomorphization driver
/// for generic-decl fields). A `Param` reaching this helper is a
/// compiler bug.
pub(crate) fn resolved_type_to_ir_type(
    ty: &ResolvedType,
    registry: &GlobalRegistry,
    instantiations: &mut Vec<Instantiation>,
) -> IRType {
    match ty.resolution {
        Resolution::Global(id) => global_to_ir_type(id, &ty.type_args, registry, instantiations),
        Resolution::TypeParam { .. } | Resolution::Local(_) | Resolution::Unresolved => panic!(
            "alpha IR lower: resolved_type_to_ir_type received a non-Global resolution \
             ({:?}) — every Param must be substituted before lowering",
            ty.resolution,
        ),
    }
}

fn global_to_ir_type(
    id: GlobalRegistryId,
    type_args: &[ResolvedType],
    registry: &GlobalRegistry,
    instantiations: &mut Vec<Instantiation>,
) -> IRType {
    let entry = registry.get(id).unwrap_or_else(|| {
        panic!("alpha IR lower: ResolvedType id {id} missing from registry — seal violation",)
    });
    if entry.identifier.is_in_package("Global") {
        assert!(
            type_args.is_empty(),
            "alpha IR lower: stdlib primitive `{}` cannot carry type_args",
            entry.identifier,
        );
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
    let template = IRSymbol::from_identifier(&entry.identifier);
    let translated: Vec<IRType> = type_args
        .iter()
        .map(|arg| resolved_type_to_ir_type(arg, registry, instantiations))
        .collect();
    if !translated.is_empty() {
        instantiations.push(Instantiation {
            template: id,
            args: type_args.to_vec(),
        });
    }
    let symbol = mangled_type_name(&template, &translated);
    match &entry.kind {
        GlobalKind::Enum(_) => IRType::Enum(symbol),
        GlobalKind::Struct(_) => IRType::Struct(symbol),
        other => panic!(
            "alpha IR lower: cannot translate `{}` ({}) to IRType yet",
            entry.identifier,
            other.label(),
        ),
    }
}
