//! Sealed IR for project-mode sources (`koja build`, `koja run` on
//! a manifest-rooted package) plus the [`lower_program`] entry point
//! that produces them.
//!
//! [`lower_program`] consumes a sealed
//! [`koja_typecheck::CheckedProgram`] and either:
//! - returns `Ok(IRProgram)` whose shape is **sealed** (every block
//!   ends in a terminator, every value reference points at a
//!   previously-defined value in the same function, the entry point
//!   resolves to a registered function), or
//! - returns `Err(LowerError)` carrying one of two user-actionable
//!   failure modes: feature-gap diagnostics accumulated while walking
//!   the sealed AST, or an entry-point lookup miss when the caller
//!   asked for a function that no package registered.
//!
//! `seal_program` runs as the last sub-pass of `lower_program`; seal
//! violations panic per northstar (compiler bugs, not user errors).

use std::collections::{BTreeMap, BTreeSet};

use koja_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_typecheck::{CheckedProgram, GlobalRegistry};

use crate::constant::IRConstantValue;
use crate::cycle::break_type_cycles;
use crate::elaborate;
use crate::enum_decl::IREnumDecl;
use crate::error::LowerError;
use crate::function::{FunctionKind, IRFunction, IRSymbol};
use crate::generics::{self, Instantiation};
use crate::lower::{
    LowerOutput, ProcessBodyTypes, resolved_type_to_ir_type, synthesize_process_entry_wrapper,
};
use crate::package::IRPackage;
use crate::struct_decl::IRStructDecl;
use crate::tail_calls::rewrite_tail_calls;
use crate::types::IRType;
use crate::union_decl::{IRUnionDecl, discover_unions};
use crate::{lower, merge, seal};

/// Caller-supplied entry shape for [`lower_program`]. Two flavors:
///
/// - [`ProjectEntry::Function`] — the user named a `fn main`-style
///   entry function. Resolved straight through:
///   `entry_point = IRSymbol::from_identifier(&ident)`. Transitional
///   path that survives only as long as v1 lives; once v1 is gone
///   the `Function` variant is deleted and PascalCase Process
///   entries become the sole project shape.
/// - [`ProjectEntry::Process`] — the user named a PascalCase state
///   type that implements `Process<C, M, R>`. `lower_program`
///   resolves its `C` (config type) off the typecheck registry,
///   synthesizes one [`FunctionKind::ProcessEntryWrapper`] thunk
///   for that state, points `entry_point` at the wrapper symbol,
///   and enqueues `state.start` / `state.run` `Instantiation`s so
///   the monomorphizer picks up generic-state cells.
#[derive(Debug, Clone)]
pub enum ProjectEntry {
    Function(Identifier),
    Process { state: Identifier },
}

/// Sealed output of [`lower_program`]'s success path. Backends consume
/// this directly; they build their own indices over the sealed
/// vocabulary and never need to revisit the `CheckedProgram` it came
/// from.
///
/// `entry_point` is the stable [`IRSymbol`] backends lift into a host
/// `main`. Two shapes route through it:
///
/// - [`ProjectEntry::Function`] caller: the symbol is the user's
///   `fn main` directly; backends inline its body.
/// - [`ProjectEntry::Process`] caller: the symbol is the synthesized
///   `<state>.__entry_wrapper` whose [`FunctionKind::ProcessEntryWrapper`]
///   tells backends to emit a spawn-driven trampoline.
///
/// `link_libraries` is the deduped, sorted list of bare library names
/// (`m`, `crypto`) collected from every `@extern "C"` function's
/// [`crate::IRExternAttrs::link_lib`]. The driver feeds these to the
/// linker as `-l<name>`. Per-function `link_name` overrides stay on
/// the [`IRFunction`] — only the library set surfaces here.
#[derive(Debug, Clone)]
pub struct IRProgram {
    pub entry_point: IRSymbol,
    pub link_libraries: Vec<String>,
    pub packages: Vec<IRPackage>,
}

impl IRProgram {
    /// Lookup a function across every package by its mangled symbol.
    /// `O(packages * log functions_per_package)`; for the 1–3 packages
    /// an program ships today this is overwhelmingly cheap. A
    /// flat index lands when codegen needs hot-path lookups.
    ///
    /// Accepts any `&str`-borrowable input, so backends can pass a
    /// `&IRSymbol` directly or a raw mangled string they pulled off
    /// an `IRInstruction::Call`.
    pub fn function(&self, mangled: &str) -> Option<&IRFunction> {
        self.packages
            .iter()
            .find_map(|pkg| pkg.functions.get(mangled))
    }

    /// Lookup a struct declaration across every package by its
    /// mangled symbol. Mirrors [`Self::function`]; backends pass a
    /// `&IRSymbol` from `IRType::Struct` / `IRInstruction::StructInit`
    /// / `IRInstruction::FieldGet` directly through the
    /// `IRSymbol: Borrow<str>` impl.
    pub fn struct_decl(&self, mangled: &str) -> Option<&IRStructDecl> {
        self.packages
            .iter()
            .find_map(|pkg| pkg.structs.get(mangled))
    }

    /// Lookup an enum declaration across every package by its
    /// mangled symbol. Mirrors [`Self::struct_decl`]; backends pass
    /// a `&IRSymbol` from `IRType::Enum` /
    /// `IRInstruction::EnumConstruct` directly through the
    /// `IRSymbol: Borrow<str>` impl.
    pub fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl> {
        self.packages.iter().find_map(|pkg| pkg.enums.get(mangled))
    }

    /// Lookup a union declaration across every package by its
    /// mangled symbol. Mirrors [`Self::struct_decl`]; backends pass
    /// the `&IRSymbol` carried on `IRType::Union { mangled }`
    /// directly through the `IRSymbol: Borrow<str>` impl.
    pub fn union_decl(&self, mangled: &str) -> Option<&IRUnionDecl> {
        self.packages.iter().find_map(|pkg| pkg.unions.get(mangled))
    }

    /// Lookup a pooled constant value across every package by its
    /// mangled symbol. Mirrors [`Self::struct_decl`]; backends pass
    /// the `&IRSymbol` carried on [`crate::IRInstruction::LoadConst`]
    /// directly through the `IRSymbol: Borrow<str>` impl.
    pub fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue> {
        self.packages
            .iter()
            .find_map(|pkg| pkg.constants.get(mangled))
    }

    /// The function the entry point resolves to. Panics if missing —
    /// the entry-point existence check is a precondition that
    /// `lower_program` enforces, and `seal_program` re-asserts on the
    /// final IRProgram.
    pub fn entry_function(&self) -> &IRFunction {
        self.function(self.entry_point.mangled())
            .expect("entry point not registered in IRProgram (seal violation upstream)")
    }

    /// Whether `function` is this program's entry point. Lets backends
    /// distinguish the entry function (which gets exported under the
    /// host-runtime symbol, e.g. `main` on Unix) from every other
    /// function in the program — symbol-keyed, with no AST types in
    /// scope.
    pub fn is_entry(&self, function: &IRFunction) -> bool {
        function.symbol == self.entry_point
    }
}

/// Run every sub-pass in the lowering phase.
///
/// Sub-pass order (forced by data dependencies):
///
/// 1. `lower_package` — translate each `CheckedPackage` into an
///    `IRPackage` fragment. Generic decls are skipped (they live in
///    the typecheck registry); concrete instantiations encountered
///    along the way accumulate into a flat list keyed at the
///    template's [`koja_ast::identifier::GlobalRegistryId`]. Feature-
///    gap diagnostics push into the shared buffer and the offending
///    decl is dropped.
/// 2. If any diagnostics were recorded, return
///    `Err(LowerError::Diagnostics)` immediately. Seal never runs on
///    a partial IR — its invariants assume a complete program, and
///    violating them panics (northstar: seal failures are compiler
///    bugs, not user errors).
/// 3. For [`ProjectEntry::Process`] callers: resolve the state's
///    `Process<C, M, R>` impl, enqueue `start`/`run` instantiations,
///    and synthesize the [`FunctionKind::ProcessEntryWrapper`]
///    thunk under `<state>.__entry_wrapper`. The wrapper is routed
///    into the state's owning package via the post-instantiate
///    drain.
/// 4. `generics::instantiate` — dedupe the instantiation list and
///    monomorphize each one off the typecheck registry into the
///    [`IRPackage`] that owns the template. The instantiation set is
///    dropped here and never reaches merge / seal / backends. The
///    drain also routes any leftover synthesized functions (the
///    entry wrapper above) to their owning packages.
/// 5. `merge` — stitch the per-package fragments into a single
///    working `IRProgram`.
/// 6. Entry-point existence check — surfaces `EntryPointNotFound`.
/// 7. `seal` — assert sealed-IRProgram invariants. Panics on violation.
pub fn lower_program(
    checked: &CheckedProgram,
    entry: ProjectEntry,
) -> Result<IRProgram, LowerError> {
    let mut output = LowerOutput::default();
    let mut packages = Vec::with_capacity(checked.packages.len() + 1);
    packages.push(empty_global_stdlib_package());
    for pkg in &checked.packages {
        packages.push(lower::lower_package(pkg, &checked.registry, &mut output));
    }

    if !output.diagnostics.is_empty() {
        return Err(LowerError::Diagnostics(output.diagnostics));
    }

    let (entry_identifier, entry_symbol) = match &entry {
        ProjectEntry::Function(ident) => (ident.clone(), IRSymbol::from_identifier(ident)),
        ProjectEntry::Process { state } => {
            stage_process_entry(state, checked, &mut packages, &mut output)?
        }
    };

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

    let mut program = merge::merge(packages, entry_symbol);
    program.link_libraries = collect_link_libraries(program.packages.iter());
    discover_unions(&mut program.packages);
    break_type_cycles(&mut program.packages);
    rewrite_tail_calls(&mut program.packages);
    elaborate::elaborate(&mut program.packages);

    if program.function(program.entry_point.mangled()).is_none() {
        return Err(LowerError::EntryPointNotFound {
            identifier: entry_identifier,
        });
    }

    seal::seal_program(&program);
    Ok(program)
}

/// Synthesize the [`FunctionKind::ProcessEntryWrapper`] for a
/// [`ProjectEntry::Process`] caller and enqueue `start` / `run`
/// instantiations. Returns the entry's user-facing identifier (the
/// state) plus the wrapper's mangled [`IRSymbol`] — `lower_program`
/// stamps the latter onto [`IRProgram::entry_point`].
///
/// The wrapper drops directly into the state's owning [`IRPackage`]
/// (no `synthesized_functions` round-trip needed for the non-generic
/// case, which is the only shape `koja.toml` can name today).
fn stage_process_entry(
    state: &Identifier,
    checked: &CheckedProgram,
    packages: &mut [IRPackage],
    output: &mut LowerOutput,
) -> Result<(Identifier, IRSymbol), LowerError> {
    let (state_id, state_entry) =
        checked
            .registry
            .lookup(state)
            .ok_or_else(|| LowerError::EntryPointNotFound {
                identifier: state.clone(),
            })?;
    let process_proto_id = checked
        .registry
        .lookup(&Identifier::new("Global", vec!["Process".to_string()]))
        .map(|(id, _)| id)
        .expect("IR lower: `Global.Process` protocol missing from registry");
    let protocol_args = checked
        .registry
        .lookup_conformance(state_id, process_proto_id)
        .ok_or_else(|| LowerError::EntryPointNotFound {
            identifier: state.clone(),
        })?
        .to_vec();
    let [config_resolved, _msg, _reply] = protocol_args.as_slice() else {
        panic!(
            "IR lower: `Process` impl for `{}` has {} type arg(s), expected 3",
            state_entry.identifier,
            protocol_args.len(),
        );
    };

    let config_type = resolved_type_to_ir_type(
        config_resolved,
        &checked.registry,
        &mut output.instantiations,
    );
    let state_resolved = ResolvedType::leaf(Resolution::Global(state_id));
    let state_ir = resolved_type_to_ir_type(
        &state_resolved,
        &checked.registry,
        &mut output.instantiations,
    );
    let state_symbol = match &state_ir {
        IRType::Struct(symbol) => symbol.clone(),
        other => panic!(
            "IR lower: Process entry `{}` must lower to a struct state, got `{other:?}`",
            state_entry.identifier,
        ),
    };

    enqueue_process_methods(state_id, &checked.registry, output);

    let body_types = ProcessBodyTypes::resolve(
        &state_resolved,
        state_ir,
        config_type,
        &checked.registry,
        output,
    );
    let [body, wrapper] = synthesize_process_entry_wrapper(&state_symbol, body_types);
    let wrapper_symbol = wrapper.symbol.clone();
    insert_into_owning_package(packages, body);
    insert_into_owning_package(packages, wrapper);

    Ok((state.clone(), wrapper_symbol))
}

fn enqueue_process_methods(
    state_id: GlobalRegistryId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) {
    let Some(state_entry) = registry.get(state_id) else {
        return;
    };
    for method in ["start", "run"] {
        let mut path = state_entry.identifier.path().to_vec();
        path.push(method.to_string());
        let method_ident = Identifier::new(state_entry.identifier.package(), path);
        if let Some((method_id, _)) = registry.lookup(&method_ident) {
            output.instantiations.push(Instantiation {
                template: method_id,
                args: Vec::new(),
                method_args: Vec::new(),
                owner: state_id,
            });
        }
    }
}

fn insert_into_owning_package(packages: &mut [IRPackage], function: IRFunction) {
    let symbol_str = function.symbol.mangled();
    let prefix = symbol_str.split('.').next().unwrap_or(symbol_str);
    let index = packages
        .iter()
        .position(|pkg| pkg.package == prefix)
        .unwrap_or(0);
    let owner = packages
        .get_mut(index)
        .expect("IR lower: no IRPackage available to host the synthesized process entry wrapper");
    owner.functions.insert(function.symbol.clone(), function);
}

/// Empty `Global` IRPackage seeded so `generics::monomorphize` has a
/// place to land stdlib stub instantiations (today only `Option<T>`).
pub(crate) fn empty_global_stdlib_package() -> IRPackage {
    IRPackage {
        constants: BTreeMap::new(),
        enums: BTreeMap::new(),
        functions: BTreeMap::new(),
        package: "Global".to_string(),
        structs: BTreeMap::new(),
        unions: BTreeMap::new(),
    }
}

/// Walk every `@extern "C"` function across `packages` and collect a
/// deduped, sorted list of `link_lib` names. Used at lower time so
/// backends and cache layers don't re-walk the IR. Functions without
/// a `link_lib` (bare `@extern "C"` with no `@link`) contribute
/// nothing; the C symbol is still resolved via the normal libc /
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
