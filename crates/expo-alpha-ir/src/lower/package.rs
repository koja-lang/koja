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
    Diagnostic, Function, ImplBlock, ImplMember, Item, Param, TypeExpr, is_extern_c, is_intrinsic,
};
use expo_ast::identifier::{GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType};

use crate::constant::IRConstantValue;
use crate::enum_decl::IREnumDecl;
use crate::extern_attrs::IRExternAttrs;
use crate::function::{FunctionKind, IRFunction, IRFunctionParam, IRInstruction, IRSymbol};
use crate::generics::Instantiation;
use crate::local::IRLocalId;
use crate::mangling::mangled_type_name;
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::types::IRType;

use super::body::{finalize_open_flow, lower_body};
use super::constants::lower_constant_pool_entry;
use super::ctx::{FnLowerCtx, LowerOutput};
use super::enums::lower_enum_decl;
use super::ownership::ownership_for_param;
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
    let mut constants: BTreeMap<IRSymbol, IRConstantValue> = BTreeMap::new();
    let mut enums: BTreeMap<IRSymbol, IREnumDecl> = BTreeMap::new();
    let mut functions: BTreeMap<IRSymbol, IRFunction> = BTreeMap::new();
    let mut structs: BTreeMap<IRSymbol, IRStructDecl> = BTreeMap::new();
    for file in &pkg.files {
        for item in &file.items {
            match item {
                Item::Constant(constant) => {
                    if let Some((symbol, value)) =
                        lower_constant_pool_entry(constant, &pkg.package, registry)
                    {
                        constants.insert(symbol, value);
                    }
                }
                Item::Enum(decl) => {
                    if let Some(lowered) = lower_enum_decl(decl, &pkg.package, registry, output) {
                        enums.insert(lowered.symbol.clone(), lowered);
                    }
                    if decl.type_params.is_empty() {
                        for function in &decl.functions {
                            let identifier = Identifier::new(
                                &pkg.package,
                                vec![decl.name.clone(), function.name.clone()],
                            );
                            if let Some(lowered) = lower_function_with_identifier(
                                function, identifier, registry, output,
                            ) {
                                functions.insert(lowered.symbol.clone(), lowered);
                            }
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
                    if decl.type_params.is_empty() {
                        for function in &decl.functions {
                            let identifier = Identifier::new(
                                &pkg.package,
                                vec![decl.name.clone(), function.name.clone()],
                            );
                            if let Some(lowered) = lower_function_with_identifier(
                                function, identifier, registry, output,
                            ) {
                                functions.insert(lowered.symbol.clone(), lowered);
                            }
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
        constants,
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
    if impl_target_is_generic(target_name, package, registry) {
        return;
    }
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

/// True when `target_name` resolves to a generic struct/enum.
/// Methods on a generic target are specialized through
/// [`crate::generics::instantiate`] when the receiver type is
/// concrete; lowering them eagerly at the template would feed
/// `TypeParam` into [`resolved_type_to_ir_type`] and panic.
fn impl_target_is_generic(target_name: &str, package: &str, registry: &GlobalRegistry) -> bool {
    let identifier = Identifier::new(package, vec![target_name.to_string()]);
    registry
        .lookup(&identifier)
        .is_some_and(|(_, entry)| !entry.type_params.is_empty())
}

/// Bare head identifier from an impl block's target. `pub(crate)` so
/// [`crate::generics`] reuses the same shape match when building the
/// AST function index. Mirrors alpha-typecheck's
/// `lift_signatures::impl_target_name` — both `impl X` and
/// `impl X<...>` shapes register their methods under `[X, method_name]`.
pub(crate) fn impl_target_name(target: &TypeExpr) -> Option<&str> {
    match target {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } if path.len() == 1 => {
            Some(path[0].as_str())
        }
        _ => None,
    }
}

/// Lower one [`Function`] under `identifier`. `@intrinsic`-annotated
/// functions become [`FunctionKind::Intrinsic`] with empty blocks
/// (backends synthesize bodies from a mangled-symbol table);
/// `@extern "C"`-annotated functions become [`FunctionKind::Extern`]
/// with empty blocks and the parsed `link_name` / `link_lib` attrs;
/// regular functions become [`FunctionKind::Regular`] with at least
/// one basic block. Returns `None` (with a diagnostic) on feature
/// gaps.
///
/// Generic functions are skipped here — same shape as the
/// generic-struct skip in [`super::structs::lower_struct_decl`].
/// Specialization happens later when [`crate::generics::instantiate`]
/// drives the worklist of [`Instantiation`]s recorded at call sites.
pub(super) fn lower_function_with_identifier(
    function: &Function,
    identifier: Identifier,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Option<IRFunction> {
    if !function.type_params.is_empty() {
        return None;
    }
    let signature = function_signature(registry, &identifier)?;
    let symbol = IRSymbol::from_identifier(&identifier);
    lower_function_inner(function, &identifier, signature, symbol, registry, output)
}

/// Body of [`lower_function_with_identifier`] minus the registry
/// signature lookup and the generic skip — both of which the
/// monomorphization driver supplies on its own (substituted
/// signature, mangled symbol). Shared by the concrete top-level
/// path and `crate::generics::monomorphize::monomorphize_function`.
pub(crate) fn lower_function_inner(
    function: &Function,
    identifier: &Identifier,
    signature: &FunctionSignature,
    symbol: IRSymbol,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Option<IRFunction> {
    let return_type =
        resolved_type_to_ir_type(&signature.return_type, registry, &mut output.instantiations);
    let intrinsic = is_intrinsic(&function.annotations);
    let extern_c = is_extern_c(&function.annotations);

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
            symbol,
        });
    }

    if extern_c {
        let params = lower_intrinsic_params(function, signature, registry, output, &mut ctx)?;
        let attrs = IRExternAttrs::from_annotations(&function.annotations);
        return Some(IRFunction {
            blocks: Vec::new(),
            kind: FunctionKind::Extern(attrs),
            params,
            return_type,
            symbol,
        });
    }

    let Some(body) = function.body.as_ref() else {
        output.diagnostics.push(Diagnostic::error(
            format!(
                "alpha IR does not yet lower bodyless fn `{identifier}` (no `@intrinsic` / \
                 `@extern \"C\"` marker — provide one or add a body)",
            ),
            function.span,
        ));
        return None;
    };

    let entry = ctx.fresh_block("entry");
    let params = lower_params(function, identifier, signature, registry, output, &mut ctx)?;

    let flow = lower_body(body, &mut ctx, entry, registry, output).ok()?;
    finalize_open_flow(&mut ctx, flow);

    let blocks = ctx.into_blocks();
    Some(IRFunction {
        blocks,
        kind: FunctionKind::Regular,
        params,
        return_type,
        symbol,
    })
}

/// Mint a [`ValueId`](crate::types::ValueId) per parameter (in
/// declaration order, `self` included) and promote each into a local
/// slot via `LocalDecl` + `LocalWrite` appended to the entry block.
/// `self` is treated as a regular param here: typecheck stamps
/// `local_id` on every param shape, and `ExprKind::Self_` references
/// read through the same `LocalRead` path body locals use.
///
/// The promotion `LocalWrite` carries the parameter's ownership
/// stamp from [`super::ownership::ownership_for_param`]: `move`
/// params (`move c: T`, `move self`) of heap-typed `T` enter their
/// slot as [`Ownership::Owned`]; default-borrow params and
/// stack-typed `move` params enter as [`Ownership::Unowned`]. This
/// wires the slot directly into the drop pipeline so a `move`
/// parameter that's never reassigned still gets a `DropLocal`
/// at function exit.
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
        let mode = signature.params[index].mode;
        let resolved = &signature.params[index].ty;
        let ty = resolved_type_to_ir_type(resolved, registry, &mut output.instantiations);
        let ownership = ownership_for_param(mode, &ty);
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
                ownership,
                value: id,
            },
        );
        ctx.mark_local_declared(ir_local, ty.clone());
        ctx.mark_local_written(ir_local, ownership);
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
        if entry.identifier.last() == "CPtr" {
            assert_eq!(
                type_args.len(),
                1,
                "alpha IR lower: stdlib primitive `Global.CPtr` requires exactly one type \
                 argument; got {} ({type_args:?})",
                type_args.len(),
            );
            let pointee = resolved_type_to_ir_type(&type_args[0], registry, instantiations);
            return IRType::CPtr(Box::new(pointee));
        }
        assert!(
            type_args.is_empty(),
            "alpha IR lower: stdlib primitive `{}` cannot carry type_args",
            entry.identifier,
        );
        return match entry.identifier.last() {
            "Bool" => IRType::Bool,
            "Float" | "Float64" => IRType::Float64,
            "Float32" => IRType::Float32,
            "Int" | "Int64" => IRType::Int64,
            "Int8" => IRType::Int8,
            "Int16" => IRType::Int16,
            "Int32" => IRType::Int32,
            // `Never` has no runtime representation. The only place
            // an alpha expression's resolution surfaces `Never` is a
            // fully-divergent `if`/`else`/`cond` whose merge block we
            // still synthesize for surrounding-flow continuity but
            // is never reached at runtime. Mapping to `Unit` is a
            // structurally-safe placeholder until `IRType::Never`
            // lands alongside `Kernel.panic` and friends.
            "Never" => IRType::Unit,
            "UInt8" => IRType::UInt8,
            "UInt16" => IRType::UInt16,
            "UInt32" => IRType::UInt32,
            "UInt64" => IRType::UInt64,
            "String" => IRType::String,
            "Unit" => IRType::Unit,
            other => panic!(
                "alpha IR lower: cannot translate `Global.{other}` to IRType yet \
                 (extend `with_stdlib_stubs` and this match in lockstep)",
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
            owner: id,
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
