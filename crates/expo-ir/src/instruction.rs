//! IR instructions and terminators -- the operations that make up a basic block.

use crate::types::{IRBuiltinOp, IROperand, IRType, IRVar};

/// A single non-terminating instruction within a basic block. Each instruction
/// produces at most one result (stored in `dest`) and consumes zero or more
/// operands. Instructions are executed sequentially within a block; control
/// flow is handled exclusively by the block's [`IRTerminator`].
#[derive(Debug, Clone, PartialEq)]
pub enum IRInstruction {
    /// Stack-allocate space for a value of the given type. The result is a
    /// pointer that can be used with `Load` and `Store`.
    Alloca { dest: IRVar, ty: IRType },

    /// Call a function by mangled name with the given arguments. If the
    /// function returns a value, `dest` captures it.
    Apply {
        args: Vec<IROperand>,
        dest: Option<IRVar>,
        function: String,
    },

    /// Begin a read-only borrow of a value. The source remains live but
    /// cannot be moved or mutated until `EndBorrow` is issued on the result.
    BorrowValue { dest: IRVar, source: IRVar },

    /// Execute a primitive arithmetic, comparison, or logical operation.
    Builtin {
        args: Vec<IROperand>,
        dest: IRVar,
        op: IRBuiltinOp,
    },

    /// Deep-copy a value, producing a new independently-owned copy. The
    /// source remains live and owned by its original scope.
    CloneValue { dest: IRVar, source: IRVar },

    /// Destroy a value at scope exit. Calls the type's destructor (if any)
    /// and frees heap-allocated memory. After this instruction, the variable
    /// is dead and must not be referenced.
    DropValue { value: IRVar },

    /// End a borrow scope started by `BorrowValue`. The borrowed reference
    /// is dead after this instruction and the original owner regains full
    /// access.
    EndBorrow { value: IRVar },

    /// Construct an enum value. For variants with a payload, `payload`
    /// carries the inner value; unit variants set it to `None`.
    Enum {
        dest: IRVar,
        payload: Option<IROperand>,
        ty: IRType,
        variant: String,
    },

    /// Heap-allocate space for a value of the given type. Used for closure
    /// environments and values that must outlive the current stack frame.
    HeapAlloc { dest: IRVar, ty: IRType },

    /// Load a value from a pointer produced by `Alloca` or `HeapAlloc`.
    Load { dest: IRVar, pointer: IRVar },

    /// Transfer ownership of a value. After this instruction the source
    /// variable is dead -- any subsequent use is a compile error.
    MoveValue { dest: IRVar, source: IRVar },

    /// Create a closure by binding an environment to a function. The result
    /// is a callable value that captures the environment.
    PartialApply {
        dest: IRVar,
        environment: IRVar,
        function: String,
    },

    /// Store a value into a pointer produced by `Alloca` or `HeapAlloc`.
    Store { pointer: IRVar, value: IROperand },

    /// Create a string constant.
    StringLiteral { dest: IRVar, value: String },

    /// Construct a struct value from its fields, in declaration order.
    Struct {
        dest: IRVar,
        fields: Vec<IROperand>,
        ty: IRType,
    },

    /// Extract a single field from a struct by name. This is a typed
    /// operation -- the backend decides the physical access method
    /// (GEP, offset, etc.).
    StructExtract {
        base: IROperand,
        dest: IRVar,
        field: String,
        ty: IRType,
    },
}

/// The final instruction in a basic block. Every block ends with exactly
/// one terminator, which transfers control to another block or returns
/// from the function.
#[derive(Debug, Clone, PartialEq)]
pub enum IRTerminator {
    /// Branch to one of two target blocks based on a boolean condition.
    CondBranch {
        condition: IROperand,
        else_block: String,
        then_block: String,
    },

    /// Unconditional jump to a target block.
    Jump(String),

    /// Return from the function, optionally with a value.
    Return(Option<IROperand>),

    /// Branch on an enum's discriminant tag. Each case maps a variant name
    /// to a target block; payloads arrive as block arguments.
    SwitchEnum {
        cases: Vec<(String, String)>,
        value: IROperand,
    },

    /// Marks a control-flow path that should never be reached. Encountering
    /// this at runtime is undefined behavior.
    Unreachable,
}
