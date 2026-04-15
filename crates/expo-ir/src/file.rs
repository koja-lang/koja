//! Top-level IR containers: files, functions, structs, and basic blocks.

use crate::instruction::{IRInstruction, IRTerminator};
use crate::types::{IRType, IRVar};

/// The top-level IR container for a single compilation unit. Produced by
/// lowering a typed AST file, consumed by a codegen backend.
#[derive(Debug, Clone)]
pub struct IRFile {
    /// All functions defined in this file, including methods (desugared to
    /// free functions with mangled names like `Point_distance`).
    pub functions: Vec<IRFunction>,
    /// The name of this compilation unit (typically the source file stem).
    pub name: String,
    /// All monomorphized struct definitions used by functions in this file.
    /// Generic structs appear once per concrete instantiation (e.g.
    /// `List_$Int32$`).
    pub structs: Vec<IRStruct>,
}

/// A monomorphized struct definition. All type parameters have been
/// replaced with concrete types during lowering.
#[derive(Debug, Clone)]
pub struct IRStruct {
    /// The struct's fields in declaration order, each with a name and
    /// resolved type.
    pub fields: Vec<(String, IRType)>,
    /// The mangled name of this struct (e.g. `"Point"` or `"Pair_$Int32.String$"`).
    pub name: String,
}

/// A function in the IR. Methods have been desugared: `self` becomes an
/// explicit first parameter, and the function name is mangled to include
/// the receiver type (e.g. `Point_distance`).
#[derive(Debug, Clone)]
pub struct IRFunction {
    /// The function body as a sequence of basic blocks. The first block
    /// is the entry point.
    pub blocks: Vec<IRBasicBlock>,
    /// The mangled function name.
    pub name: String,
    /// The function's parameters in declaration order, each with a name
    /// and resolved type.
    pub parameters: Vec<(String, IRType)>,
    /// The function's return type.
    pub return_type: IRType,
}

/// A basic block: a straight-line sequence of instructions ending with a
/// terminator that transfers control elsewhere. Blocks may receive
/// arguments (e.g. payloads delivered by `SwitchEnum`).
#[derive(Debug, Clone)]
pub struct IRBasicBlock {
    /// Values received from predecessor blocks (e.g. enum payloads from
    /// `SwitchEnum`). Empty for most blocks.
    pub arguments: Vec<IRVar>,
    /// The instructions executed sequentially within this block, before
    /// the terminator.
    pub instructions: Vec<IRInstruction>,
    /// The block's label, used as a branch target (e.g. `"entry"`,
    /// `"loop_head"`, `"then"`).
    pub label: String,
    /// The final instruction that transfers control to another block or
    /// returns from the function.
    pub terminator: IRTerminator,
}
