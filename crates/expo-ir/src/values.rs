//! Operand model for ExpoIR: SSA value identifiers, operands, and the
//! instruction enum.
//!
//! ## Design
//!
//! - [`IRValueId`] is the SSA-style handle for a value produced inside
//!   a function body. Function-scoped, opaque to lowering and emission
//!   alike. Minted by [`crate::FnLowerState::next_value_id`].
//! - [`IROperand`] is what instruction and terminator slots hold when
//!   they want to refer to a value. Either a previously-produced
//!   [`IRValueId`] (via `Local`) or an inline literal constant.
//!   Literals do not need an instruction to produce them.
//! - [`IRInstruction`] is the per-block instruction enum. It carries
//!   typed variants for each [`expo_ast::ast::ExprKind`] that has
//!   learned to lower, plus a transitional [`IRInstruction::Stub`]
//!   that bridges to AST-level expression emission for kinds that
//!   haven't lifted yet. Each future Expr kind retires `Stub` for
//!   that kind by introducing a typed variant and replacing `Stub` at
//!   its lowering site. When the last consumer is gone, `Stub` is
//!   deleted in one PR.
//!
//! ## Why a transitional `Stub` variant
//!
//! The same rationale that justified Wave 11's AST-stub bodies on
//! [`crate::resolved::conditionals::IRUnless`] applies one level
//! finer: the IR scaffolding lands ahead of the full instruction set
//! so each construct can lift in isolation. The alternative -- block
//! every operand-shaped slot until the entire instruction set is
//! defined -- would force a single mega-slice that designs the IR
//! against speculation rather than real consumers.
//!
//! Side tables were considered (and rejected) for the bridge: they
//! divorce execution order from the instruction stream and require
//! the consumer to consult two stores. A first-class `Stub` variant
//! keeps the stream single-source-of-truth and gives the migration a
//! clear, greppable retirement marker.

use expo_ast::ast::Expr;
use expo_typecheck::types::Type;

use crate::blocks::IRBlockId;
use crate::identity::FunctionIdentifier;
use crate::resolved::fields::ResolvedFieldStep;
use crate::resolved::ops::{ResolvedBinaryOp, ResolvedUnaryOp};

/// Function-scoped SSA value identifier. Minted by
/// [`crate::FnLowerState::next_value_id`]. Per-function counters
/// reset at function entry, so ids are only meaningful within their
/// owning function's lowering/emission context.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct IRValueId(pub u32);

/// What an instruction or terminator references when it wants a
/// value. Either a previously-produced [`IRValueId`] or an inline
/// literal constant.
///
/// Constants do not require an instruction to produce them. Lowering
/// emits the literal variants directly; emission materializes them
/// to backend constants on demand.
#[derive(Clone, Debug)]
pub enum IROperand {
    /// Boolean literal. Emitted by lowering when [`crate::lower::values::lower_expr_to_operand`]
    /// recognizes a `true` / `false` literal in operand position.
    ConstBool(bool),
    /// Floating-point literal.
    ConstFloat(f64),
    /// Integer literal.
    ConstInt(i64),
    /// String literal.
    ConstStr(String),
    /// Reference to a value produced earlier in the same function by
    /// an [`IRInstruction`].
    Local(IRValueId),
    /// The unit value. Backends materialize this however their unit
    /// representation requires (a zero-sized struct, an `i8 0`, etc.).
    Unit,
}

/// A single instruction in a basic block's instruction sequence.
///
/// Variants are alpha-sorted. The transitional [`IRInstruction::Stub`]
/// variant bridges to AST-level expression emission for kinds that
/// haven't lifted yet; each future Expr kind that learns to lower
/// replaces its `Stub` site with a typed instruction variant. When
/// the last consumer is gone, `Stub` is deleted.
#[derive(Clone, Debug)]
pub enum IRInstruction {
    /// Binary arithmetic, comparison, or logical operation. The
    /// [`ResolvedBinaryOp`] variant fully encodes both operand kind
    /// (Int vs Float vs String) and result kind (comparisons -> Bool,
    /// arithmetic -> operand kind), so emission needs no further
    /// decision logic.
    ///
    /// Reaches lowering via [`crate::lower::values::lower_expr_to_operand`]
    /// dispatching on [`expo_ast::ast::ExprKind::Binary`]. Concat
    /// (multi-block memcpy) and `EnumStructEqual` (multi-block
    /// per-variant equality) are not handled by this variant -- they
    /// fall through to [`IRInstruction::Stub`] until they get
    /// dedicated instruction variants.
    BinaryOp {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Resolved operation -- maps 1:1 to a single LLVM builder call.
        op: ResolvedBinaryOp,
        /// Left-hand operand.
        lhs: IROperand,
        /// Right-hand operand.
        rhs: IROperand,
    },
    /// Direct or static-method function call. Encodes the resolved
    /// mangled symbol, the lowered argument operands, and the
    /// resolved parameter / return types so emission can materialize
    /// each argument, coerce it to the matching parameter type, and
    /// emit the LLVM call without further resolution work.
    ///
    /// Reaches lowering via [`crate::lower::values::lower_expr_to_operand`]
    /// dispatching on [`expo_ast::ast::ExprKind::Call`], or via the
    /// codegen wrappers (`compile_call`, `compile_static_call`) that
    /// attempt the lift before their legacy emission paths.
    /// Builtin (`panic` / `print*`), closure-variable, generic, and
    /// struct-constructor calls fall through to [`IRInstruction::Stub`]
    /// because they require codegen-side state the IR-level lift does
    /// not see.
    Call {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Resolved callee symbol, registered in
        /// [`crate::program::IRProgram`].
        mangled: FunctionIdentifier,
        /// Lowered argument operands, parallel to `param_types`.
        args: Vec<IROperand>,
        /// Resolved parameter types -- the emission walker coerces
        /// each materialized argument to the matching entry.
        param_types: Vec<Type>,
        /// Callee's resolved return type. Carried alongside the
        /// destination so wrappers can re-attach a typed value at
        /// the materialization seam.
        return_type: Type,
    },
    /// Static GEP chain on a field-access path rooted at a named
    /// local (`a.b.c`, `self.origin.x`). Carries the chain's base
    /// binding name, its resolved type, and the sequence of
    /// per-hop field steps. The codegen executor delegates to
    /// `expo-codegen`'s `emit_chain_field_access`, which walks the
    /// alloca with a single GEP chain and one final load -- no
    /// per-hop scratch allocas.
    ///
    /// Reaches lowering via [`crate::lower::values::lower_expr_to_operand`]
    /// dispatching on [`expo_ast::ast::ExprKind::FieldAccess`] when
    /// [`crate::lower::fields::resolve_chain_steps`] succeeds.
    /// Receivers that don't resolve to a named-local-rooted chain
    /// (e.g. `make_pair().left`) lower to [`IRInstruction::FieldLoad`]
    /// instead.
    FieldChain {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Name of the chain's root binding (e.g. `"self"`, `"point"`).
        /// Resolves to a storage pointer in the codegen-side variables
        /// map at emission time.
        base_name: String,
        /// Resolved type of the root binding. The first GEP step uses
        /// this type to locate its struct layout.
        base_type: Type,
        /// Each successive field hop in the chain. The codegen walker
        /// GEPs through them in order on the root pointer, then issues
        /// one final `load` (or `load_maybe_indirect` for indirect
        /// fields) on the resulting pointer.
        steps: Vec<ResolvedFieldStep>,
    },
    /// Struct field load. Materializes the receiver as a struct
    /// value, then projects out one field at the resolved index.
    /// Used when the receiver does **not** root at a named local --
    /// e.g. `make_pair().left`, where the receiver is a call result.
    /// Named-local-rooted chains lower to [`IRInstruction::FieldChain`]
    /// instead, restoring the static-chain GEP optimization.
    ///
    /// For non-chain receivers, `base` is an opaque struct value and
    /// emission necessarily round-trips through an entry-block scratch
    /// alloca (one per hop). LLVM's mem2reg / SROA cleans these up.
    FieldLoad {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Receiver operand. Resolves to a struct value at
        /// materialization time.
        base: IROperand,
        /// Resolved field hop -- index into the struct layout plus
        /// the field's [`expo_ast::types::Type`]. Embedded directly
        /// so emission needs no further lookups.
        step: ResolvedFieldStep,
    },
    /// Load a module-level `const` value into an SSA slot. The
    /// codegen executor reads the precomputed [`inkwell::values::BasicValueEnum`]
    /// out of `Compiler.constants` and binds it to `dest`.
    ///
    /// Reaches lowering via [`crate::lower::values::lower_expr_to_operand`]
    /// dispatching on [`expo_ast::ast::ExprKind::Ident`] when the
    /// name is registered in
    /// [`expo_typecheck::context::TypeContext::constants`].
    LoadConst {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Source-level constant name (the registry key in
        /// `Compiler.constants` and `type_ctx.constants`).
        name: String,
        /// Declared type of the constant. Carried so backends that
        /// don't reach into `type_ctx` still have full type info.
        ty: Type,
    },
    /// Load a named local binding into an SSA slot. The codegen
    /// executor looks up the binding's storage pointer in
    /// `Compiler.fn_state.variables` and emits the appropriate
    /// `build_load` for the binding's type.
    ///
    /// Reaches lowering via [`crate::lower::values::lower_expr_to_operand`]
    /// dispatching on [`expo_ast::ast::ExprKind::Ident`] (when the
    /// name resolves to an in-scope binding) or
    /// [`expo_ast::ast::ExprKind::Self_`] (always, with `name = "self"`).
    LoadLocal {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Source-level binding name (the key into
        /// `Compiler.fn_state.variables`).
        name: String,
        /// Resolved Expo type of the binding. Drives the load's LLVM
        /// type at emission.
        ty: Type,
    },
    /// Build a closure-compatible fat-pointer (`{ fn_ptr, env_ptr }`)
    /// for a top-level function reference, so the function name can
    /// flow through any code path that expects a callable value
    /// (closure-typed parameters, `Ident`-as-value, etc.). The
    /// codegen executor calls `Compiler::get_or_create_thunk` and
    /// pairs the thunk with a null environment pointer.
    ///
    /// Reaches lowering via [`crate::lower::values::lower_expr_to_operand`]
    /// dispatching on [`expo_ast::ast::ExprKind::Ident`] when the
    /// name resolves to a function in
    /// [`expo_typecheck::context::TypeContext::functions`] but not to
    /// a local binding or a constant.
    MakeFnRef {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Source-level function name (the registry key in
        /// `Compiler.module.get_function` and `type_ctx.functions`).
        name: String,
        /// Resolved [`Type::Function`] for the reference. Carried so
        /// backends that don't reach into `type_ctx` still have full
        /// type info.
        fn_type: Type,
    },
    /// Instance method call (`receiver.method(args)`). The receiver
    /// is materialized first and passed as the implicit `self`
    /// argument; subsequent operands are coerced against
    /// `param_types[1..]`. `is_move` and `receiver_name` carry the
    /// existing ownership-tracking contract: when the resolved method
    /// consumes its receiver by-move and the receiver expression is
    /// a named local, the emission walker marks that variable
    /// `Ownership::Unowned` after the call.
    ///
    /// Reaches lowering via [`crate::lower::values::lower_expr_to_operand`]
    /// dispatching on [`expo_ast::ast::ExprKind::MethodCall`], or via
    /// `compile_method_call`'s lift attempt. Self-tail-recursive
    /// calls (TCO), generic methods needing inference,
    /// pending-monomorphization, and the field-typed-as-function
    /// closure invocation path all fall through to
    /// [`IRInstruction::Stub`].
    MethodCall {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Resolved callee symbol, registered in
        /// [`crate::program::IRProgram`].
        mangled: FunctionIdentifier,
        /// Receiver operand, materialized as the implicit `self`.
        receiver: IROperand,
        /// Receiver variable name when the receiver expression is a
        /// simple [`expo_ast::ast::ExprKind::Ident`] or
        /// [`expo_ast::ast::ExprKind::Self_`]. `None` for
        /// non-named receivers (chained calls, expression results).
        /// Used together with `is_move` to update the receiver's
        /// ownership in the per-function variables map.
        receiver_name: Option<String>,
        /// Whether the resolved method consumes the receiver
        /// by-move ([`expo_typecheck::context::PassMode::Move`]).
        is_move: bool,
        /// Lowered argument operands (excluding the receiver),
        /// parallel to `param_types[1..]`.
        args: Vec<IROperand>,
        /// Resolved parameter types. `param_types[0]` is the
        /// receiver type (no coercion applied -- receiver type is
        /// concrete after resolution); `param_types[1..]` cover the
        /// non-self arguments.
        param_types: Vec<Type>,
        /// Callee's resolved return type.
        return_type: Type,
    },
    /// SSA value merge at a join point. Each `(block_id, operand)`
    /// pair contributes one incoming edge; the codegen executor
    /// materializes `build_phi(llvm_ty, name)` then walks `incomings`
    /// issuing `add_incoming((value, llvm_block))`. The instruction's
    /// `dest` becomes the phi's SSA result, consumable downstream
    /// via [`IROperand::Local`].
    ///
    /// Reaches lowering via [`crate::lower::conditionals::Lowerer::lower_ternary`]
    /// (pre-staged in `IRTernary::merge_instructions` because both
    /// arms are pure expressions and their values are known at
    /// lowering time) and via the codegen-side
    /// `emit_if_else` walker (synthesized at emit time when both
    /// statement-bodied arms produce a value).
    ///
    /// Phi requires the LLVM block context to call `add_incoming`,
    /// so [`crate::values::IRInstruction::Phi`] only walks correctly
    /// when [`crate::values::IRInstruction`] flow through
    /// `execute_instructions` with a populated block map. Conditional
    /// constructs that don't contain a Phi (`unless`, `if`-no-else)
    /// keep passing `None` for the block map.
    Phi {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Predecessor edges: each tuple supplies the value
        /// contributed when control reaches the join from that
        /// block. Ordering is irrelevant to LLVM but conventionally
        /// follows the lowering's branch-target order.
        incomings: Vec<(IRBlockId, IROperand)>,
        /// Resolved Expo type of the merged value. Drives the LLVM
        /// type passed to `build_phi`.
        ty: Type,
    },
    /// **Transitional.** Bridges to AST-level expression emission
    /// while the rest of the instruction set fills in. The emission
    /// walker computes the LLVM value for `expr` via
    /// `compile_expr` and registers it under `dest` in the per-block
    /// value map. Subsequent operands referencing `IROperand::Local(dest)`
    /// resolve via the same map.
    ///
    /// Retirement: as each [`expo_ast::ast::ExprKind`] learns to
    /// lower, replace its `Stub` site with a typed `IRInstruction`
    /// variant. When the last consumer is gone, this variant is
    /// deleted. Greppable on the symbol `IRInstruction::Stub`.
    Stub {
        /// SSA destination this instruction produces. Subsequent
        /// operands reference it via [`IROperand::Local`].
        dest: IRValueId,
        /// AST expression to evaluate at emission time. Boxed
        /// because [`Expr`] is large (~280 bytes) and would
        /// otherwise dominate the enum's discriminant size.
        expr: Box<Expr>,
    },
    /// Unary negation or logical-not. The [`ResolvedUnaryOp`] variant
    /// encodes both the operand kind (Int vs Float) and which LLVM
    /// builder call to issue.
    UnaryOp {
        /// SSA destination this instruction produces.
        dest: IRValueId,
        /// Resolved operation -- maps 1:1 to a single LLVM builder call.
        op: ResolvedUnaryOp,
        /// Operand to apply the unary op to.
        operand: IROperand,
    },
}

impl IRInstruction {
    /// SSA destination this instruction writes. Useful for emission
    /// walkers populating a `HashMap<IRValueId, _>`.
    pub fn dest(&self) -> IRValueId {
        match self {
            IRInstruction::BinaryOp { dest, .. }
            | IRInstruction::Call { dest, .. }
            | IRInstruction::FieldChain { dest, .. }
            | IRInstruction::FieldLoad { dest, .. }
            | IRInstruction::LoadConst { dest, .. }
            | IRInstruction::LoadLocal { dest, .. }
            | IRInstruction::MakeFnRef { dest, .. }
            | IRInstruction::MethodCall { dest, .. }
            | IRInstruction::Phi { dest, .. }
            | IRInstruction::Stub { dest, .. }
            | IRInstruction::UnaryOp { dest, .. } => *dest,
        }
    }
}
