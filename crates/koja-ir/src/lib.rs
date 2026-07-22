//! Sealed intermediate representation for Koja.
//!
//! [`COMPILER-NORTHSTAR.md`]: ../../design/COMPILER-NORTHSTAR.md
//!
//! Two lowering entry points serve project and script execution:
//!
//! - [`lower_program`] lowers a project and synthesizes the entry
//!   wrapper for the requested concrete `Process` state.
//! - [`lower_script`] lowers top-level script statements directly.
//!
//! Both consume a sealed [`koja_typecheck::CheckedProgram`] and return
//! a fully transformed, seal-validated output. [`LowerError`] carries
//! user-facing lowering failures. Seal failures are compiler bugs.

mod binary_packing;
mod cfg;
mod constant;
mod cycle;
mod dominators;
mod elaborate;
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
mod package;
mod program;
mod script;
mod seal;
mod struct_decl;
mod tail_calls;
mod types;
mod union_decl;
mod yield_checks;

pub use binary_packing::pack_integer_segment;
pub use constant::IRConstantValue;
pub use enum_decl::{EnumPayloadInit, IREnumDecl, IREnumVariant, IRVariantPayload, IRVariantTag};
pub use error::LowerError;
pub use extern_attrs::IRExternAttrs;
pub use function::{
    BlockParam, BranchTarget, FunctionKind, IRBasicBlock, IRBlockId, IRFunction, IRFunctionParam,
    IRIndirectSlot, IRInstruction, IRSourceDef, IRSymbol, IRTerminator, ReceiveAfter, ReceiveArm,
    ReceiveTag,
};
pub use intrinsic_id::{
    BinaryMethod, BitOp, BitsMethod, CPtrMethod, CStringMethod, DebugImpl, EqualityImpl, FloatType,
    HashImpl, IRIntrinsicId, IntNarrowTarget, IntType, KernelMethod, ListMethod, MapMethod,
    NumericConvert, ParseTarget, ProcessMethod, RefMethod, ReplyToMethod, RuntimeBlockMethod,
    SetMethod, SocketMethod, StringMethod,
};
pub use local::IRLocalId;
pub use package::IRPackage;
pub use program::{IRProgram, lower_program};
pub use script::{IRScript, lower_script};
pub use struct_decl::{IRStructDecl, IRStructField, StructFieldInit};
pub use tail_calls::function_has_tail_call;
pub use types::{
    BinaryEndian, BinarySign, ConcatKind, ConstValue, IRBinOp, IRType, IRUnaryOp,
    LoweredBinaryMatchLayout, LoweredBinaryPattern, LoweredBinarySegment, NEG_OVERFLOW_MESSAGE,
    ResolvedBinaryLayout, ValueId,
};
pub use union_decl::IRUnionDecl;
