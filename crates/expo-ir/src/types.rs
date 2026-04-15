//! Core IR type primitives: types, variables, operands, and builtin operations.

use expo_ast::identifier::TypeIdentifier;
use expo_ast::types::Primitive;

/// A resolved type in the IR. All generic type parameters have been
/// monomorphized away by the lowering pass -- every `IRType` is concrete.
/// Reuses `TypeIdentifier` and `Primitive` from `expo-ast` so the IR stays
/// consistent with the rest of the compiler pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IRType {
    /// A function type with parameter types and a return type.
    Function {
        /// The types of each parameter, in declaration order.
        parameter_types: Vec<IRType>,
        /// The return type of the function.
        return_type: Box<IRType>,
    },
    /// A user-defined named type (struct or enum), fully monomorphized.
    Named(TypeIdentifier),
    /// A built-in primitive type with a known size and representation.
    Primitive(Primitive),
    /// A borrowed reference to a value. The referent remains owned by the
    /// original scope; the borrow must end before the owner is dropped.
    Ref(Box<IRType>),
    /// The unit type, representing the absence of a meaningful value.
    Unit,
}

/// An SSA variable. Each variable is assigned exactly once and identified
/// by a unique index within its function. The optional name preserves the
/// source-level name for diagnostics and debug output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IRVar {
    /// Unique index within the containing function (0-based).
    pub id: usize,
    /// Source-level name, if one exists (e.g. `"x"` for `let x = ...`).
    /// Compiler-generated temporaries have `None`.
    pub name: Option<String>,
}

/// A value that can appear as an argument to an instruction. Either a
/// reference to an SSA variable or a compile-time constant.
#[derive(Debug, Clone, PartialEq)]
pub enum IROperand {
    /// A boolean constant (`true` or `false`).
    ConstBool(bool),
    /// A floating-point constant.
    ConstFloat(f64),
    /// An integer constant.
    ConstInt(i64),
    /// A string constant.
    ConstStr(String),
    /// The unit value `()`.
    Unit,
    /// A reference to an SSA variable produced by a prior instruction.
    Var(IRVar),
}

/// A primitive arithmetic, comparison, or logical operation. These map
/// directly to single machine instructions on most targets. The type
/// suffix indicates the operand type -- the IR is explicit about whether
/// an operation is integer or floating-point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IRBuiltinOp {
    /// Floating-point addition.
    AddFloat,
    /// Integer addition.
    AddInt,
    /// Boolean AND (short-circuiting is handled at the control-flow level).
    And,
    /// Floating-point division.
    DivFloat,
    /// Integer division.
    DivInt,
    /// Floating-point equality comparison.
    EqFloat,
    /// Integer equality comparison.
    EqInt,
    /// Floating-point greater-than-or-equal comparison.
    GeFloat,
    /// Integer greater-than-or-equal comparison.
    GeInt,
    /// Floating-point greater-than comparison.
    GtFloat,
    /// Integer greater-than comparison.
    GtInt,
    /// Floating-point less-than-or-equal comparison.
    LeFloat,
    /// Integer less-than-or-equal comparison.
    LeInt,
    /// Floating-point less-than comparison.
    LtFloat,
    /// Integer less-than comparison.
    LtInt,
    /// Floating-point multiplication.
    MulFloat,
    /// Integer multiplication.
    MulInt,
    /// Floating-point inequality comparison.
    NeFloat,
    /// Integer inequality comparison.
    NeInt,
    /// Floating-point negation.
    NegFloat,
    /// Integer negation (two's complement).
    NegInt,
    /// Integer bitwise NOT.
    NotInt,
    /// Boolean OR (short-circuiting is handled at the control-flow level).
    Or,
    /// Floating-point remainder.
    RemFloat,
    /// Integer remainder.
    RemInt,
    /// Floating-point subtraction.
    SubFloat,
    /// Integer subtraction.
    SubInt,
}
