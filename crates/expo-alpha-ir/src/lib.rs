//! Sealed alpha-IR lowering for the [`COMPILER-NORTHSTAR.md`] pipeline.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../design/COMPILER-NORTHSTAR.md
//!
//! Two public entry points, one IR shape per usecase:
//!
//! - [`lower_program`] consumes a sealed
//!   [`expo_alpha_typecheck::CheckedProgram`] whose source declared an
//!   entry function (`fn main`) and returns a sealed [`IRProgram`].
//!   This is the project-mode path; the entry point is named by an
//!   [`expo_ast::identifier::Identifier`].
//! - [`lower_script`] consumes a sealed
//!   [`expo_alpha_typecheck::CheckedProgram`] whose source was parsed
//!   in `ParseMode::Script` (top-level statements live on
//!   [`expo_ast::ast::File::body`]) and returns a sealed [`IRScript`].
//!   This is the script-mode path; the script body *is* the entry
//!   point — there's no identifier for it.
//!
//! Both paths share the same [`IRPackage`] / [`IRFunction`] vocabulary
//! for helper-function decls and the same per-function lowering
//! helpers; the difference is only in the entry-point shape they
//! produce. Diagnostics: lowering is a pure translation from a sealed
//! input. User-actionable errors funnel through [`LowerError`];
//! everything else is a compiler bug and panics through `seal`.
//!
//! Hard contract: this crate has **zero dependency on `expo-ir`**. The
//! IR vocabulary defined here ([`IRProgram`], [`IRScript`],
//! [`IRPackage`], [`IRFunction`], [`IRInstruction`], [`IRTerminator`],
//! …) is fresh and self-contained.

mod cfg;
mod constant;
mod dominators;
mod enum_decl;
mod error;
mod extern_attrs;
mod function;
mod generics;
mod intrinsic_id;
mod local;
mod lower;
pub mod mangling;
mod merge;
mod ownership;
mod package;
mod program;
mod script;
mod seal;
mod struct_decl;
mod tail_calls;
mod types;
mod union_decl;

pub use constant::IRConstantValue;
pub use enum_decl::{EnumPayloadInit, IREnumDecl, IREnumVariant, IRVariantPayload, IRVariantTag};
pub use error::LowerError;
pub use extern_attrs::IRExternAttrs;
pub use function::{
    BlockParam, BranchTarget, FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRFunctionParam,
    IRInstruction, IRSymbol, IRTerminator, ReceiveAfter, ReceiveArm, ReceiveTag,
};
pub use intrinsic_id::{
    BinaryMethod, BitOp, BitsMethod, CPtrMethod, CStringMethod, DebugImpl, EqualityImpl, HashImpl,
    IRIntrinsicId, IntType, KernelMethod, ListMethod, MapMethod, ParseTarget, RefMethod,
    ReplyToMethod, SetMethod, SocketMethod, StringMethod,
};
pub use local::IRLocalId;
pub use ownership::Ownership;
pub use package::IRPackage;
pub use program::{IRProgram, lower_program};
pub use script::{IRScript, lower_script};
pub use struct_decl::{IRStructDecl, IRStructField, StructFieldInit};
pub use tail_calls::function_has_tail_call;
pub use types::{
    BinaryEndian, BinarySign, ConcatKind, ConstValue, IRBinOp, IRType, IRUnaryOp,
    LoweredBinarySegment, ResolvedBinaryLayout, ValueId,
};
pub use union_decl::{IRUnionDecl, size_in_bytes};
