//! Sealed-AST -> IR lowering, organized as one submodule per
//! concern. Public surface to the rest of the crate is [`lower_package`]
//! (the package walker invoked from [`crate::program::lower_program`])
//! and [`lower_body_to_blocks`] (the body-shaped seam
//! [`crate::lower_script`] also reuses).
//!
//! Layout map:
//!
//! - [`constants`]: registry-backed [`crate::constant::IRConstantValue`] lowering and pool
//!   admission helpers used by [`package`] when filling [`crate::package::IRPackage::constants`]
//!   and by [`expr`] for `LoadConst` vs inline `Const`.
//! - [`ctx`]: [`FnLowerCtx`] + [`FlowResult`], the per-function
//!   accumulator every helper threads through. No language-aware
//!   logic, just counters, the [`crate::cfg::CFGBuilder`], and the
//!   `value -> IRType` index.
//! - [`package`]: the entry points [`lower_package`] /
//!   [`lower_function`] plus the registry adapters
//!   ([`function_signature`], [`resolved_type_to_ir_type`]) that bridge
//!   the typecheck-resolved AST to the IR vocabulary.
//! - [`body`]: the statement-list driver ([`lower_body_to_blocks`],
//!   [`lower_body`], [`lower_statement`], [`finalize_open_flow`]).
//!   Owns the `Statement::Return` handling and the per-function
//!   fail-fast contract.
//! - [`control_flow`]: `if` / `unless` lowering. Builds the
//!   then/merge (or body/merge) blocks, stamps `CondBranch` /
//!   `Branch` terminators, and emits the merge `Const::Unit`
//!   placeholder for the conditional's value.
//! - [`drops`]: function-exit drop emission. Appends one
//!   [`crate::IRInstruction::DropLocal`] per Live & Owned slot
//!   into the block immediately before the function-exit
//!   terminator. Invoked from the return-path lowerer and the
//!   fall-through finalizer.
//! - [`calls`]: call-site lowering, [`calls::lower_call`] (bare
//!   calls) + [`calls::lower_method_call`] (instance / static
//!   method calls), with a shared `emit_call` tail.
//! - [`expr`]: expression dispatch. [`expr::lower_expr`] fans out
//!   to every other submodule.
//! - [`ops`]: operator / literal translation ([`lower_literal`],
//!   [`lower_bin_op`], [`lower_unary_op`]), plus the small
//!   `IRType`-shaped result-type helpers.
//! - [`structs`]: struct decl, struct-literal construction, and
//!   field-access lowering ([`lower_struct_decl`],
//!   [`lower_struct_construction`], [`lower_field_access`]).

mod arms;
mod binary_literal;
mod binary_match;
mod body;
mod calls;
mod closures;
mod constants;
mod control_flow;
mod ctx;
mod drops;
mod enums;
mod expr;
mod list_literal;
mod loops;
mod map_literal;
mod match_expr;
mod ops;
mod ownership;
pub(crate) mod package;
mod patterns;
mod process;
mod structs;
mod tuples;

pub(crate) use body::lower_body_to_blocks;
pub(crate) use ctx::LowerOutput;
pub(crate) use package::{lower_package, resolved_type_to_ir_type};
pub(crate) use process::{ProcessBodyTypes, synthesize_process_entry_wrapper};
