//! Sealed-AST -> IR lowering, one submodule per language concern.
//! [`package`] holds the entry points [`lower_package`] and
//! [`package::lower_function_inner`]. [`body`] drives statement lists, [`expr`]
//! dispatches expressions to the submodule named after each form,
//! and every helper threads the per-function [`ctx::FnLowerCtx`].

mod arms;
mod binary_literal;
mod binary_match;
mod bind_detach;
mod body;
mod calls;
mod closures;
mod constants;
mod control_flow;
mod ctx;
mod drops;
mod enums;
mod equality;
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
mod unions;

pub(crate) use body::lower_body_to_blocks;
pub(crate) use ctx::LowerOutput;
pub(crate) use package::{lower_package, resolved_type_to_ir_type};
pub(crate) use process::{ProcessBodyTypes, synthesize_process_entry_wrapper};
