//! Package- and function-shaped lowering entry points. Walks a
//! sealed [`CheckedPackage`] into an [`IRPackage`] fragment, delegating
//! per-function body work to [`super::body`]. Also owns the
//! [`GlobalRegistry`] adapters ([`function_signature`],
//! [`resolved_type_to_ir_type`]) so siblings import a stable seam.
//!
//! Top-level / inline-struct / `impl`-block functions all flow
//! through [`lower_function_with_identifier`]. Only the
//! [`Identifier`] differs.

use koja_ast::ast::{
    Diagnostic, ExtendBlock, Function, ImplBlock, ImplMember, Item, Param, TypeExpr, is_extern_c,
    is_intrinsic,
};
use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType,
};
use koja_typecheck::{
    BuiltinShape, CheckedPackage, FunctionDefinition, FunctionSignature, GlobalKind, GlobalRegistry,
};

use crate::constant::IRConstantValue;
use crate::enum_decl::IREnumDecl;
use crate::extern_attrs::IRExternAttrs;
use crate::function::{FunctionKind, IRFunction, IRFunctionParam, IRSourceDef, IRSymbol};
use crate::generics::Instantiation;
use crate::intrinsic_id::IRIntrinsicId;
use crate::local::IRLocalId;
use crate::mangling::{mangled_type_name, source_function_symbol, union_mangle};
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
/// Generic struct / enum decls are skipped here. They live in the
/// typecheck registry and only become concrete decls when
/// [`crate::generics::instantiate`] specializes them. Concrete
/// instantiations encountered while lowering construction sites,
/// field types, or function signatures append to
/// `output.instantiations` for the driver to monomorphize, while
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
                // A builtin's representation is compiler-provided
                // (`builtin_to_ir_type`), so the decl emits no IR
                // struct. Methods on generic builtins specialize
                // through monomorphization like any generic type.
                Item::Builtin(decl) => {
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
/// Unsupported targets already errored upstream, so IR silently skips
/// them. Synthesized default-method bodies lower like any other
/// method. Functions key off the target's qualified identifier
/// regardless of the impl's own package (cross-package impls carry a
/// local protocol onto a foreign type), and the IR doesn't model the
/// trait link.
fn lower_impl(
    impl_block: &ImplBlock,
    package: &str,
    def_file: Option<&Path>,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
    functions: &mut BTreeMap<IRSymbol, IRFunction>,
) {
    let Some(path) = nominal_target_path(&impl_block.target) else {
        return;
    };
    let Some((_, target_package, target_path)) = registry.lookup_owner_path(path, package) else {
        return;
    };
    if impl_target_is_generic(&target_path, target_package.as_str(), registry) {
        return;
    }
    lower_block_members(
        target_package.as_str(),
        &target_path,
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
    let Some((_, target_package, target_path)) = registry.lookup_owner_path(path, package) else {
        return;
    };
    // Protocol targets carry statics only (typecheck enforces this),
    // so their extends lower eagerly even though the protocol itself
    // is generic. The generic skip is for struct/enum receivers whose
    // methods mention the target's type params.
    if !extend_target_is_protocol(&target_path, target_package.as_str(), registry)
        && impl_target_is_generic(&target_path, target_package.as_str(), registry)
    {
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

fn extend_target_is_protocol(
    target_path: &[String],
    package: &str,
    registry: &GlobalRegistry,
) -> bool {
    let identifier = Identifier::new(package, target_path.to_vec());
    registry
        .lookup(&identifier)
        .is_some_and(|(_, entry)| matches!(entry.kind, GlobalKind::Protocol(_)))
}

/// Shared member-lowering loop for [`lower_impl`] and [`lower_extend`].
/// `fn` members key at `<target_package>.<target_name>.<method>`,
/// and type aliases are dropped.
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
/// concrete. Lowering them eagerly at the template would feed
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
/// (backends synthesize bodies from a mangled-symbol table).
/// `@extern "C"`-annotated functions become [`FunctionKind::Extern`]
/// with empty blocks and the parsed `link_name` / `link_lib` attrs,
/// and regular functions become [`FunctionKind::Regular`] with at least
/// one basic block. Returns `None` (with a diagnostic) on feature
/// gaps.
///
/// Generic functions are skipped here, the same shape as the
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
    let definition = function_definition(registry, &identifier, function.params.len())?;
    let signature = definition.signature.as_ref().unwrap_or_else(|| {
        panic!(
            "IR lower: function `{identifier}/{}` has no lifted signature",
            function.params.len()
        )
    });
    let symbol = source_function_symbol(&identifier, definition.arity);
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
/// signature lookup and the generic skip, both of which the
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
                 `@extern \"C\"` marker, provide one or add a body)",
            ),
            function.span,
        ));
        return None;
    };

    let entry = ctx.fresh_block("entry");
    let params = lower_params(function, identifier, signature, registry, output, &mut ctx)?;

    let flow = lower_body(body, &mut ctx, entry, registry, output).ok()?;
    finalize_open_flow(&mut ctx, flow, &return_type);

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
/// `self` is treated as a regular param here, since typecheck stamps
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
                "IR lower: `{identifier}` parameter #{index} carries no `LocalId`, \
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
/// promotion. Backends synthesize the body and never walk the
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
                "IR lower: intrinsic parameter #{index} carries no `LocalId` \
                 (typecheck resolve invariant violation)",
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

/// Look up one exact function definition.
pub(super) fn function_definition<'a>(
    registry: &'a GlobalRegistry,
    identifier: &Identifier,
    arity: usize,
) -> Option<&'a FunctionDefinition> {
    let (_, entry) = registry.lookup_function(identifier, arity)?;
    match &entry.kind {
        GlobalKind::Function(definition) => Some(definition),
        other => panic!(
            "IR lower: function `{identifier}/{arity}` has no lifted definition \
             ({}), lift_signatures invariant violation",
            other.label(),
        ),
    }
}

/// Translate a typecheck [`ResolvedType`] to a concrete [`IRType`].
/// Stdlib `Global.{Bool,Float,Int,String,Unit}` map to scalar
/// [`IRType`]s. User structs / enums map to [`IRType::Struct`] /
/// [`IRType::Enum`], with concrete `type_args` folded into the
/// symbol via [`mangled_type_name`]. Every non-empty-args
/// translation also pushes an [`Instantiation`] (keyed at the
/// template's [`GlobalRegistryId`]) for the
/// [`crate::generics::instantiate`] driver to specialize.
///
/// Panics on `Resolution::TypeParam`, because by the time IR lowers a
/// type, every `Param` should have been substituted by the caller
/// (typecheck for resolved expressions, the monomorphization driver
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
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => IRType::Tuple(
            elements
                .iter()
                .map(|e| resolved_type_to_ir_type(e, registry, instantiations))
                .collect(),
        ),
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            type_args,
        } => global_to_ir_type(*id, type_args, registry, instantiations),
        ResolvedType::Named {
            resolution,
            type_args,
        } => panic!(
            "IR lower: resolved_type_to_ir_type received a non-Global resolution \
             ({resolution:?}), every Param must be substituted before lowering \
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
            panic!("IR lower: resolved_type_to_ir_type received Unresolved (seal violation)",)
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
        panic!("IR lower: ResolvedType id {id} missing from registry (seal violation)",)
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
    // Builtins lower structurally from their registry-stamped shape.
    // User-style stdlib structs (`DateTime`, `Duration`, etc. from
    // auto-imported `Global.*` files) and enums (`Option<T>`) fall
    // through to the generic monomorphization path.
    if let GlobalKind::Builtin(definition) = &entry.kind {
        return builtin_to_ir_type(definition.shape, id, type_args, registry, instantiations);
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

/// Structural lowering for a [`BuiltinShape`]. Generic shapes
/// (`CPtr`, `List`, `Map`, `Set`) push an [`Instantiation`] because
/// method monomorphization needs the entry even though the type
/// itself carries no struct decl: call sites mangle method symbols
/// as `List_$T$.method`, which mono materializes via
/// `enqueue_member_methods`.
fn builtin_to_ir_type(
    shape: BuiltinShape,
    id: GlobalRegistryId,
    type_args: &[ResolvedType],
    registry: &GlobalRegistry,
    instantiations: &mut Vec<Instantiation>,
) -> IRType {
    assert_eq!(
        type_args.len(),
        shape.arity(),
        "IR lower: builtin shape {shape:?} requires exactly {} type argument(s); \
         got {} ({type_args:?})",
        shape.arity(),
        type_args.len(),
    );
    if shape.arity() > 0 {
        instantiations.push(Instantiation {
            template: id,
            args: type_args.to_vec(),
            method_args: Vec::new(),
            owner: id,
        });
    }
    let mut arg = |index: usize| {
        Box::new(resolved_type_to_ir_type(
            &type_args[index],
            registry,
            instantiations,
        ))
    };
    match shape {
        BuiltinShape::Binary => IRType::Binary,
        BuiltinShape::Bits => IRType::Bits,
        BuiltinShape::Bool => IRType::Bool,
        BuiltinShape::CPtr => IRType::CPtr(arg(0)),
        BuiltinShape::Float32 => IRType::Float32,
        BuiltinShape::Float64 => IRType::Float64,
        BuiltinShape::Int8 => IRType::Int8,
        BuiltinShape::Int16 => IRType::Int16,
        BuiltinShape::Int32 => IRType::Int32,
        BuiltinShape::Int64 => IRType::Int64,
        BuiltinShape::List => IRType::List(arg(0)),
        BuiltinShape::Map => IRType::Map {
            key: arg(0),
            value: arg(1),
        },
        // `Never` has no runtime representation. The only place an
        // expression's resolution surfaces `Never` is a fully-
        // divergent `if`/`else`/`cond` whose merge block we still
        // synthesize for surrounding-flow continuity but is never
        // reached at runtime. Mapping to `Unit` is a structurally-
        // safe placeholder until `IRType::Never` lands alongside
        // `Kernel.panic` and friends.
        BuiltinShape::Never => IRType::Unit,
        BuiltinShape::Set => IRType::Set(arg(0)),
        BuiltinShape::String => IRType::String,
        BuiltinShape::UInt8 => IRType::UInt8,
        BuiltinShape::UInt16 => IRType::UInt16,
        BuiltinShape::UInt32 => IRType::UInt32,
        BuiltinShape::UInt64 => IRType::UInt64,
        BuiltinShape::Unit => IRType::Unit,
    }
}
