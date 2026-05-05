//! Sealed IR for script-mode sources (`expo run <bare-file>`,
//! `expo eval`, REPL fragments).
//!
//! Where [`crate::IRProgram`] models a user-declared entry function
//! by [`Identifier`], an `IRScript` carries its body inline: the
//! top-level statements lowered into a single basic block sequence
//! plus the package fragments needed to resolve any helper-function
//! calls. There is no "entry point identifier" — the script *is* the
//! entry point.
//!
//! Backends consume an `IRScript` directly:
//!
//! - The interpreter (`expo-alpha-ir-eval`) drives `script.blocks`
//!   through the same instruction walker it uses for an
//!   `IRFunction.blocks`, looking up callees in `script.packages`.
//! - The LLVM backend (`expo-alpha-ir-llvm`) emits `script.blocks`
//!   as the body of a host-runtime `main` function and walks
//!   `script.packages` for non-entry function declarations.
//!
//! The shape mirrors a single function's body without leaking an
//! [`IRFunction`] (which carries a name, parameters, and the
//! "user-declared" semantics that scripts deliberately don't have).

use expo_ast::identifier::Identifier;

use crate::function::{IRBasicBlock, IRFunction};
use crate::package::IRPackage;
use crate::types::IRType;

/// Sealed output of [`crate::lower_script`]'s success path.
///
/// `blocks` is the implicit function's body — the top-level
/// statements of the script source lowered to one or more basic
/// blocks. Today's scope produces exactly one block ending in
/// `IRTerminator::Return`, mirroring `IRFunction.blocks` for a body
/// without control flow.
///
/// `packages` carries the same per-package function fragments that
/// [`crate::IRProgram::packages`] does, so backends can resolve
/// `IRInstruction::Call` callees without revisiting the typecheck
/// registry.
///
/// `return_type` is the static type of the script's trailing
/// expression value (or `IRType::Unit` for an empty / non-expression
/// trailing statement). Backends consume this directly to size the
/// `main` return slot and the `Return` terminator's value width.
#[derive(Debug, Clone)]
pub struct IRScript {
    pub blocks: Vec<IRBasicBlock>,
    pub packages: Vec<IRPackage>,
    pub return_type: IRType,
}

impl IRScript {
    /// Lookup a helper function across every package by its
    /// fully-qualified [`Identifier`]. Mirrors
    /// [`crate::IRProgram::function`] so the interpreter and LLVM
    /// backend can drive a single shared instruction walker over
    /// either IR shape — only the call-resolver closure differs.
    pub fn function(&self, id: &Identifier) -> Option<&IRFunction> {
        self.packages.iter().find_map(|pkg| pkg.functions.get(id))
    }
}
