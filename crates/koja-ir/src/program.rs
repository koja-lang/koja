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
//!   named a state type that no package registered (or that lacks a
//!   `Process` conformance).
//!
//! `seal_program` runs as the last sub-pass of `lower_program`. Seal
//! violations panic per northstar (compiler bugs, not user errors).

use koja_ast::identifier::{GlobalRegistryId, Identifier, Resolution, ResolvedType};
use koja_typecheck::{CheckedProgram, GlobalRegistry};

use crate::constant::IRConstantValue;
use crate::declarations::{Declarations, find_in};
use crate::enum_decl::IREnumDecl;
use crate::error::LowerError;
use crate::function::{IRFunction, IRSymbol};
use crate::generics::Instantiation;
use crate::lower::{
    LowerOutput, ProcessBodyTypes, resolved_type_to_ir_type, synthesize_process_entry_wrapper,
};
use crate::package::{IRPackage, insert_package_function};
use crate::pipeline;
use crate::struct_decl::IRStructDecl;
use crate::types::IRType;
use crate::union_decl::IRUnionDecl;
use crate::{merge, seal};

/// Sealed output of [`lower_program`]'s success path. Backends consume
/// this directly. They build their own indices over the sealed
/// vocabulary and never need to revisit the `CheckedProgram` it came
/// from.
///
/// `entry_point` is the stable [`IRSymbol`] backends lift into a host
/// `main`: the synthesized `<state>.__entry_wrapper` whose
/// [`FunctionKind::ProcessEntryWrapper`] tells backends to emit a
/// spawn-driven trampoline.
///
/// `link_libraries` is the deduped, sorted list of bare library names
/// (`m`, `crypto`) collected from every `@extern "C"` function's
/// [`crate::IRExternAttrs::link_lib`]. The driver feeds these to the
/// linker as `-l<name>`. Per-function `link_name` overrides stay on
/// the [`IRFunction`]. Only the library set surfaces here.
///
/// `built_constant_order` lists every [`IRConstantValue::Built`]
/// constant once, each after the constants its init reaches through
/// the call graph. Backends run the inits in this order before the
/// entry point starts. [`IRPackage::constants`] is a `BTreeMap`, so
/// this list is the only place that order survives.
#[derive(Debug, Clone)]
pub struct IRProgram {
    pub built_constant_order: Vec<IRSymbol>,
    pub entry_point: IRSymbol,
    pub link_libraries: Vec<String>,
    pub packages: Vec<IRPackage>,
}

impl IRProgram {
    /// One index over every declaration in the program, for passes
    /// that look up many symbols.
    pub(crate) fn declarations(&self) -> Declarations<'_> {
        Declarations::new(&self.packages)
    }

    /// Lookup a function across every package by its mangled symbol.
    /// Accepts any `&str`-borrowable input, so backends can pass a
    /// `&IRSymbol` directly or a raw mangled string they pulled off
    /// an `IRInstruction::Call`.
    pub fn function(&self, mangled: &str) -> Option<&IRFunction> {
        find_in(&self.packages, |pkg| &pkg.functions, mangled)
    }

    /// Lookup a struct declaration across every package by its
    /// mangled symbol. Mirrors [`Self::function`]. Backends pass a
    /// `&IRSymbol` from `IRType::Struct` / `IRInstruction::StructInit`
    /// / `IRInstruction::FieldGet` directly through the
    /// `IRSymbol: Borrow<str>` impl.
    pub fn struct_decl(&self, mangled: &str) -> Option<&IRStructDecl> {
        find_in(&self.packages, |pkg| &pkg.structs, mangled)
    }

    /// Lookup an enum declaration across every package by its
    /// mangled symbol. Mirrors [`Self::struct_decl`]. Backends pass
    /// a `&IRSymbol` from `IRType::Enum` /
    /// `IRInstruction::EnumConstruct` directly through the
    /// `IRSymbol: Borrow<str>` impl.
    pub fn enum_decl(&self, mangled: &str) -> Option<&IREnumDecl> {
        find_in(&self.packages, |pkg| &pkg.enums, mangled)
    }

    /// Lookup a union declaration across every package by its
    /// mangled symbol. Mirrors [`Self::struct_decl`]. Backends pass
    /// the `&IRSymbol` carried on `IRType::Union { mangled }`
    /// directly through the `IRSymbol: Borrow<str>` impl.
    pub fn union_decl(&self, mangled: &str) -> Option<&IRUnionDecl> {
        find_in(&self.packages, |pkg| &pkg.unions, mangled)
    }

    /// Lookup a pooled constant value across every package by its
    /// mangled symbol. Mirrors [`Self::struct_decl`]. Backends pass
    /// the `&IRSymbol` carried on [`crate::IRInstruction::LoadConst`]
    /// directly through the `IRSymbol: Borrow<str>` impl.
    pub fn constant_value(&self, mangled: &str) -> Option<&IRConstantValue> {
        find_in(&self.packages, |pkg| &pkg.constants, mangled)
    }

    /// The function the entry point resolves to. Panics if missing,
    /// because the entry-point existence check is a precondition that
    /// `lower_program` enforces, and `seal_program` re-asserts on the
    /// final IRProgram.
    pub fn entry_function(&self) -> &IRFunction {
        self.function(self.entry_point.mangled())
            .expect("entry point not registered in IRProgram (seal violation upstream)")
    }

    /// Whether `function` is this program's entry point. Lets backends
    /// distinguish the entry function (which gets exported under the
    /// host-runtime symbol, e.g. `main` on Unix) from every other
    /// function in the program, symbol-keyed, with no AST types in
    /// scope.
    pub fn is_entry(&self, function: &IRFunction) -> bool {
        function.symbol == self.entry_point
    }
}

/// Lower a project through package coalescing, entry synthesis,
/// generic specialization, cycle breaking, tail-call rewriting,
/// yield insertion, ownership elaboration, and sealing.
///
/// Diagnostics return before sealing. Every synthesized declaration
/// is inserted into its explicit owner package.
pub fn lower_program(
    checked: &CheckedProgram,
    entry_state: &Identifier,
) -> Result<IRProgram, LowerError> {
    let mut output = LowerOutput::default();
    let mut packages = pipeline::lower_packages(checked, &mut output);
    pipeline::check_diagnostics(&mut output)?;

    let (entry_identifier, entry_symbol) =
        stage_process_entry(entry_state, checked, &mut packages, &mut output)?;
    pipeline::instantiate(&mut packages, checked, &mut output)?;

    let mut program = merge::merge(packages, entry_symbol);
    program.link_libraries = pipeline::collect_link_libraries(program.packages.iter());
    pipeline::rewrite(&mut program.packages, &mut []);
    program.built_constant_order =
        pipeline::built_constant_order(&program.packages, &checked.registry)?;

    if program.function(program.entry_point.mangled()).is_none() {
        return Err(LowerError::EntryPointNotFound {
            identifier: entry_identifier,
        });
    }

    seal::seal_program(&program);
    Ok(program)
}

/// Synthesize the [`FunctionKind::ProcessEntryWrapper`] for the
/// entry state and enqueue `start` / `run` instantiations. Returns
/// the entry's user-facing identifier (the state) plus the wrapper's
/// mangled [`IRSymbol`]. `lower_program` stamps the latter onto
/// [`IRProgram::entry_point`].
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
        .lookup(&Identifier::single("Global", "Process"))
        .map(|(id, _)| id)
        .expect("IR lower: `Global.Process` protocol missing from registry");
    // The entry state is non-generic (the only shape `koja.toml` can
    // name), so its `Process` record covers the empty instantiation.
    let protocol_args = checked
        .registry
        .lookup_conformance(state_id, process_proto_id, &[])
        .ok_or_else(|| LowerError::EntryPointNotFound {
            identifier: state.clone(),
        })?
        .protocol_args
        .clone();
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
    let owner_package = state_entry.identifier.package().to_string();

    enqueue_process_methods(state_id, &checked.registry, output);

    let body_types = ProcessBodyTypes::resolve(
        &state_resolved,
        state_ir,
        config_type,
        &checked.registry,
        output,
    );
    let [body, wrapper] =
        synthesize_process_entry_wrapper(&state_symbol, body_types, &checked.registry, output);
    let wrapper_symbol = wrapper.symbol.clone();
    insert_package_function(packages, &owner_package, body);
    insert_package_function(packages, &owner_package, wrapper);

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
    for method in ["priority", "run", "start"] {
        let method_ident = Identifier::member(
            state_entry.identifier.package(),
            state_entry.identifier.path(),
            method,
        );
        if let Some((method_id, _)) = registry.lookup_function(&method_ident, 1) {
            output.instantiations.push(Instantiation {
                template: method_id,
                args: Vec::new(),
                method_args: Vec::new(),
                owner: state_id,
            });
        }
    }
}
