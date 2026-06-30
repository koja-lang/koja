//! Package- and function-shaped lowering entry points. Walks a
//! sealed [`CheckedPackage`] into an [`IRPackage`] fragment, delegating
//! per-function body work to [`super::body`]. Also owns the
//! [`GlobalRegistry`] adapters ([`function_signature`],
//! [`resolved_type_to_ir_type`]) so siblings import a stable seam.
//!
//! Top-level / inline-struct / `impl`-block functions all flow
//! through [`lower_function_with_identifier`] — only the
//! [`Identifier`] differs.

use koja_ast::ast::{
    Diagnostic, ExtendBlock, Function, ImplBlock, ImplMember, Item, Param, TypeExpr, is_extern_c,
    is_intrinsic,
};
use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType,
};
use koja_typecheck::{CheckedPackage, FunctionSignature, GlobalKind, GlobalRegistry};

use crate::constant::IRConstantValue;
use crate::enum_decl::IREnumDecl;
use crate::extern_attrs::IRExternAttrs;
use crate::function::{FunctionKind, IRFunction, IRFunctionParam, IRSourceDef, IRSymbol};
use crate::generics::Instantiation;
use crate::intrinsic_id::IRIntrinsicId;
use crate::local::IRLocalId;
use crate::mangling::{mangled_type_name, union_mangle};
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::types::IRType;

use super::body::{finalize_open_flow, lower_body};
use super::constants::lower_constant_pool_entry;
use super::ctx::{FnLowerCtx, LowerOutput};
use super::enums::lower_enum_decl;
use super::ownership::promote_param;
use super::structs::lower_struct_decl;

use std::collections::BTreeMap;
use std::path::Path;

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
        let def_file = file.path.as_deref();
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
                            let identifier =
                                Identifier::member(&pkg.package, &decl.path, &function.name);
                            if let Some(lowered) = lower_function_with_identifier(
                                function, identifier, def_file, registry, output,
                            ) {
                                functions.insert(lowered.symbol.clone(), lowered);
                            }
                        }
                    }
                }
                Item::Function(function) => {
                    let identifier = Identifier::new(&pkg.package, vec![function.name.clone()]);
                    if let Some(lowered) = lower_function_with_identifier(
                        function, identifier, def_file, registry, output,
                    ) {
                        functions.insert(lowered.symbol.clone(), lowered);
                    }
                }
                Item::Struct(decl) => {
                    if let Some(lowered) = lower_struct_decl(decl, &pkg.package, registry, output) {
                        structs.insert(lowered.symbol.clone(), lowered);
                    }
                    if decl.type_params.is_empty() {
                        for function in &decl.functions {
                            let identifier =
                                Identifier::member(&pkg.package, &decl.path, &function.name);
                            if let Some(lowered) = lower_function_with_identifier(
                                function, identifier, def_file, registry, output,
                            ) {
                                functions.insert(lowered.symbol.clone(), lowered);
                            }
                        }
                    }
                }
                Item::Impl(impl_block) => {
                    lower_impl(
                        impl_block,
                        &pkg.package,
                        def_file,
                        registry,
                        output,
                        &mut functions,
                    );
                }
                Item::Extend(extend_block) => {
                    lower_extend(
                        extend_block,
                        &pkg.package,
                        def_file,
                        registry,
                        output,
                        &mut functions,
                    );
                }
                _ => {}
            }
        }
    }
    for synthesized in output.synthesized_functions.drain(..) {
        functions.insert(synthesized.symbol.clone(), synthesized);
    }
    IRPackage {
        constants,
        enums,
        functions,
        package: pkg.package.clone(),
        structs,
        unions: BTreeMap::new(),
    }
}

/// Lower methods declared in an `impl Trait for Type ... end` block.
/// Unsupported targets already errored upstream; IR silently skips
/// them. Synthesized default-method bodies lower like any other
/// method — they all register at `Package.Type.method` and the IR
/// doesn't model the trait link.
fn lower_impl(
    impl_block: &ImplBlock,
    package: &str,
    def_file: Option<&Path>,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
    functions: &mut BTreeMap<IRSymbol, IRFunction>,
) {
    let Some(target_path) = nominal_target_path(&impl_block.target) else {
        return;
    };
    if impl_target_is_generic(target_path, package, registry) {
        return;
    }
    lower_block_members(
        package,
        target_path,
        &impl_block.members,
        def_file,
        registry,
        output,
        functions,
    );
}

/// Lower methods in an `extend Type ... end` block. Functions key
/// off the target's qualified identifier regardless of the file's
/// own package, keeping dispatch stable across extending packages.
fn lower_extend(
    extend_block: &ExtendBlock,
    package: &str,
    def_file: Option<&Path>,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
    functions: &mut BTreeMap<IRSymbol, IRFunction>,
) {
    let Some(path) = nominal_target_path(&extend_block.target) else {
        return;
    };
    let Some((target_package, target_path)) = lookup_owner_path(path, package, registry) else {
        return;
    };
    if impl_target_is_generic(&target_path, target_package.as_str(), registry) {
        return;
    }
    lower_block_members(
        target_package.as_str(),
        &target_path,
        &extend_block.members,
        def_file,
        registry,
        output,
        functions,
    );
}

/// Resolve a nominal `impl`/`extend` target into its owning
/// `(package, path)`. Twin of typecheck's `lookup_owner_path`: a
/// same-package nested type wins over the `<package>.<rest>` reading.
pub(crate) fn lookup_owner_path(
    path: &[String],
    current_package: &str,
    registry: &GlobalRegistry,
) -> Option<(String, Vec<String>)> {
    if registry
        .lookup(&Identifier::new(current_package, path.to_vec()))
        .is_some()
    {
        return Some((current_package.to_string(), path.to_vec()));
    }
    if path.len() >= 2
        && registry
            .lookup(&Identifier::new(&path[0], path[1..].to_vec()))
            .is_some()
    {
        return Some((path[0].clone(), path[1..].to_vec()));
    }
    None
}

/// Shared member-lowering loop for [`lower_impl`] and [`lower_extend`].
/// `fn` members key at `<target_package>.<target_name>.<method>`;
/// type aliases are dropped.
fn lower_block_members(
    target_package: &str,
    target_path: &[String],
    members: &[ImplMember],
    def_file: Option<&Path>,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
    functions: &mut BTreeMap<IRSymbol, IRFunction>,
) {
    for member in members {
        let ImplMember::Function(function) = member else {
            continue;
        };
        let identifier = Identifier::member(target_package, target_path, &function.name);
        if let Some(lowered) =
            lower_function_with_identifier(function, identifier, def_file, registry, output)
        {
            functions.insert(lowered.symbol.clone(), lowered);
        }
    }
}

/// True when `target_path` resolves to a generic struct/enum.
/// Methods on a generic target are specialized through
/// [`crate::generics::instantiate`] when the receiver type is
/// concrete; lowering them eagerly at the template would feed
/// `TypeParam` into [`resolved_type_to_ir_type`] and panic.
fn impl_target_is_generic(
    target_path: &[String],
    package: &str,
    registry: &GlobalRegistry,
) -> bool {
    let identifier = Identifier::new(package, target_path.to_vec());
    registry
        .lookup(&identifier)
        .is_some_and(|(_, entry)| !entry.type_params.is_empty())
}

/// The dotted type path of an `impl`/`extend` target. `pub(crate)` so
/// [`crate::generics`] reuses the same shape match when building the
/// AST function index.
pub(crate) fn nominal_target_path(target: &TypeExpr) -> Option<&[String]> {
    match target {
        TypeExpr::Named { path, .. } | TypeExpr::Generic { path, .. } => Some(path.as_slice()),
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
    def_file: Option<&Path>,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Option<IRFunction> {
    if !function.type_params.is_empty() {
        return None;
    }
    let signature = function_signature(registry, &identifier)?;
    let symbol = IRSymbol::from_identifier(&identifier);
    lower_function_inner(
        function,
        &identifier,
        signature,
        symbol,
        def_file,
        registry,
        output,
    )
}

/// Build the DWARF source location for a user-declared `function`,
/// given the path of the file it was parsed from. `None` when the
/// file has no path (in-memory source) so synthetic and pathless
/// callables stay unattributed.
pub(crate) fn def_location_of(function: &Function, def_file: Option<&Path>) -> Option<IRSourceDef> {
    def_file.map(|path| IRSourceDef {
        file: path.to_path_buf(),
        line: function.span.start.line,
    })
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
    def_file: Option<&Path>,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Option<IRFunction> {
    let return_type =
        resolved_type_to_ir_type(&signature.return_type, registry, &mut output.instantiations);
    let def_location = def_location_of(function, def_file);
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
    ctx.closures_mut().set_enclosing_symbol(symbol.clone());

    if intrinsic {
        let Some(intrinsic_id) = IRIntrinsicId::from_identifier(identifier) else {
            output.diagnostics.push(Diagnostic::error(
                format!(
                    "`@intrinsic` on `{identifier}` has no registered backend handler; \
                     add a variant to `IRIntrinsicId` and wire its emitter in both backends",
                ),
                function.span,
            ));
            return None;
        };
        let params = lower_intrinsic_params(function, signature, registry, output, &mut ctx)?;
        return Some(IRFunction {
            blocks: Vec::new(),
            def_location,
            kind: FunctionKind::Intrinsic(intrinsic_id),
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
            def_location,
            kind: FunctionKind::Extern(attrs),
            params,
            return_type,
            symbol,
        });
    }

    let Some(body) = function.body.as_ref() else {
        output.diagnostics.push(Diagnostic::error(
            format!(
                "IR does not yet lower bodyless fn `{identifier}` (no `@intrinsic` / \
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
        def_location,
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
                "IR lower: `{identifier}` parameter #{index} carries no `LocalId` — \
                 typecheck resolve must stamp one for every param before lower runs",
            )
        });
        let resolved = &signature.params[index].ty;
        let ty = resolved_type_to_ir_type(resolved, registry, &mut output.instantiations);
        let ir_local = IRLocalId::from_local_id(local_id);
        let entry = ctx.entry_block();
        params.push(promote_param(ctx, entry, ir_local, ty));
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
                "IR lower: intrinsic parameter #{index} carries no `LocalId` — \
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
            "IR lower: function `{identifier}` has no lifted signature \
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
    match ty {
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => IRType::Function {
            params: params
                .iter()
                .map(|p| resolved_type_to_ir_type(p, registry, instantiations))
                .collect(),
            ret: Box::new(resolved_type_to_ir_type(ret, registry, instantiations)),
        },
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            type_args,
        } => global_to_ir_type(*id, type_args, registry, instantiations),
        ResolvedType::Named {
            resolution,
            type_args,
        } => panic!(
            "IR lower: resolved_type_to_ir_type received a non-Global resolution \
             ({resolution:?}) — every Param must be substituted before lowering \
             (type_args: {type_args:?})",
        ),
        ResolvedType::Union(members) => {
            let ir_members: Vec<IRType> = members
                .iter()
                .map(|m| resolved_type_to_ir_type(m, registry, instantiations))
                .collect();
            IRType::Union {
                mangled: union_mangle(&ir_members),
                members: ir_members,
            }
        }
        ResolvedType::Unresolved => {
            panic!("IR lower: resolved_type_to_ir_type received Unresolved — seal violation",)
        }
    }
}

fn global_to_ir_type(
    id: GlobalRegistryId,
    type_args: &[ResolvedType],
    registry: &GlobalRegistry,
    instantiations: &mut Vec<Instantiation>,
) -> IRType {
    let entry = registry.get(id).unwrap_or_else(|| {
        panic!("IR lower: ResolvedType id {id} missing from registry — seal violation",)
    });
    // Peel through `type X = ...` aliases first. Aliases stay as
    // `Named { Global(alias_id) }` in the typecheck output to keep
    // diagnostics reading `X`, not the expansion. At IR-lower time
    // we have to follow them so backends see the underlying shape.
    if let GlobalKind::TypeAlias(Some(expansion)) = &entry.kind {
        assert!(
            type_args.is_empty(),
            "IR lower: parameterized type aliases not yet supported \
             (alias `{}` was given {} type arg(s))",
            entry.identifier,
            type_args.len(),
        );
        return resolved_type_to_ir_type(expansion, registry, instantiations);
    }
    // Stdlib *primitive* `Struct(_)` stubs (scalars, `CPtr<T>`) need
    // fixed-shape lowering; user-style stdlib structs (`DateTime`,
    // `Duration`, etc. from auto-imported `Global.*` files) and
    // stdlib `Enum(_)` stubs (today `Option<T>`) fall through to
    // the generic monomorphization path. The match below is the
    // sole authority on which `Global.*` names are primitive — if
    // you add a new primitive to `with_stdlib_stubs`, add it here.
    if entry.identifier.is_in_package("Global") && matches!(entry.kind, GlobalKind::Struct(_)) {
        let last = entry.identifier.last();
        if last == "CPtr" {
            assert_eq!(
                type_args.len(),
                1,
                "IR lower: stdlib primitive `Global.CPtr` requires exactly one type \
                 argument; got {} ({type_args:?})",
                type_args.len(),
            );
            let pointee = resolved_type_to_ir_type(&type_args[0], registry, instantiations);
            // Method monomorphization needs an Instantiation entry even
            // though the pointer itself doesn't carry a struct decl —
            // call sites mangle method symbols as `CPtr_$T$.method`,
            // which mono materializes via `enqueue_member_methods`.
            instantiations.push(Instantiation {
                template: id,
                args: type_args.to_vec(),
                method_args: Vec::new(),
                owner: id,
            });
            return IRType::CPtr(Box::new(pointee));
        }
        if last == "List" {
            assert_eq!(
                type_args.len(),
                1,
                "IR lower: stdlib primitive `Global.List` requires exactly one type \
                 argument; got {} ({type_args:?})",
                type_args.len(),
            );
            let element = resolved_type_to_ir_type(&type_args[0], registry, instantiations);
            instantiations.push(Instantiation {
                template: id,
                args: type_args.to_vec(),
                method_args: Vec::new(),
                owner: id,
            });
            return IRType::List(Box::new(element));
        }
        if last == "Map" {
            assert_eq!(
                type_args.len(),
                2,
                "IR lower: stdlib primitive `Global.Map` requires exactly two type \
                 arguments; got {} ({type_args:?})",
                type_args.len(),
            );
            let key = resolved_type_to_ir_type(&type_args[0], registry, instantiations);
            let value = resolved_type_to_ir_type(&type_args[1], registry, instantiations);
            instantiations.push(Instantiation {
                template: id,
                args: type_args.to_vec(),
                method_args: Vec::new(),
                owner: id,
            });
            return IRType::Map {
                key: Box::new(key),
                value: Box::new(value),
            };
        }
        if last == "Set" {
            assert_eq!(
                type_args.len(),
                1,
                "IR lower: stdlib primitive `Global.Set` requires exactly one type \
                 argument; got {} ({type_args:?})",
                type_args.len(),
            );
            let element = resolved_type_to_ir_type(&type_args[0], registry, instantiations);
            instantiations.push(Instantiation {
                template: id,
                args: type_args.to_vec(),
                method_args: Vec::new(),
                owner: id,
            });
            return IRType::Set(Box::new(element));
        }
        let primitive = match last {
            "Binary" => Some(IRType::Binary),
            "Bits" => Some(IRType::Bits),
            "Bool" => Some(IRType::Bool),
            "Float" | "Float64" => Some(IRType::Float64),
            "Float32" => Some(IRType::Float32),
            "Int" | "Int64" => Some(IRType::Int64),
            "Int8" => Some(IRType::Int8),
            "Int16" => Some(IRType::Int16),
            "Int32" => Some(IRType::Int32),
            // `Never` has no runtime representation. The only place
            // an expression's resolution surfaces `Never` is a
            // fully-divergent `if`/`else`/`cond` whose merge block we
            // still synthesize for surrounding-flow continuity but
            // is never reached at runtime. Mapping to `Unit` is a
            // structurally-safe placeholder until `IRType::Never`
            // lands alongside `Kernel.panic` and friends.
            "Never" => Some(IRType::Unit),
            "UInt8" => Some(IRType::UInt8),
            "UInt16" => Some(IRType::UInt16),
            "UInt32" => Some(IRType::UInt32),
            "UInt64" => Some(IRType::UInt64),
            "String" => Some(IRType::String),
            "Unit" => Some(IRType::Unit),
            _ => None,
        };
        if let Some(ir) = primitive {
            assert!(
                type_args.is_empty(),
                "IR lower: stdlib primitive `{}` cannot carry type_args",
                entry.identifier,
            );
            return ir;
        }
        // Falls through to the generic struct path below for
        // user-style `Global.*` structs from the auto-import.
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
            method_args: Vec::new(),
            owner: id,
        });
    }
    let symbol = mangled_type_name(&template, &translated);
    match &entry.kind {
        GlobalKind::Enum(_) => IRType::Enum(symbol),
        GlobalKind::Struct(_) => IRType::Struct(symbol),
        other => panic!(
            "IR lower: cannot translate `{}` ({}) to IRType yet",
            entry.identifier,
            other.label(),
        ),
    }
}
