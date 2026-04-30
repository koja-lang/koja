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

use expo_ast::ast::{BinarySegment, Expr, Pattern};
use expo_typecheck::types::Type;

use crate::blocks::IRBlockId;
use crate::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier};
use crate::ownership::Ownership;
use crate::resolved::fields::ResolvedFieldStep;
use crate::resolved::ops::{ResolvedBinaryOp, ResolvedUnaryOp};
use crate::resolved::patterns::ResolvedLiteral;

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
        /// Whether this call is in tail position. Set by
        /// [`crate::lower::values::Lowerer::lower_tail_expr_to_operand`]
        /// (and its IR-level callers) when the call is the immediate
        /// expression of a `return` / last-statement-implicit-return.
        /// Plain function calls do not currently support TCO, so this
        /// is metadata only -- threaded through for consistency with
        /// [`IRInstruction::MethodCall::tail`] and to make the
        /// tail-context lift surface symmetric.
        tail: bool,
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
        /// Whether this call is in tail position. Set by
        /// [`crate::lower::values::Lowerer::lower_tail_expr_to_operand`]
        /// (and its IR-level callers) when the call is the immediate
        /// expression of a `return` / last-statement-implicit-return.
        /// The codegen executor reads this and performs the TCO
        /// rewrite (back-edge to the function's `tco_loop` block,
        /// args stored into `param_allocas`) when the call is also
        /// self-recursive. Replaces the legacy ambient
        /// `FnLowerState::tail_position` flag retired in Slice 6
        /// Wave 25.
        tail: bool,
    },
    /// Compile a binary-pattern match (multi-segment match against
    /// raw bytes) at the IR seam. Wraps `compile_binary_pattern`'s
    /// existing logic without further decomposition because binary
    /// patterns themselves are a multi-block algorithm with their own
    /// internal control flow. Produces `i1`.
    ///
    /// Reaches lowering via [`crate::lower::patterns::Lowerer::lower_pattern_to_instructions`]
    /// dispatching on `Pattern::Binary`. Subject pointer is supplied
    /// via `subject_ptr` (an [`IROperand::Local`] referencing the
    /// match's subject alloca or a recursively-projected field
    /// alloca).
    PatternBinaryMatch {
        /// SSA destination this instruction produces (the pattern's `i1`).
        dest: IRValueId,
        /// Pointer-typed operand referencing the subject's storage.
        subject_ptr: IROperand,
        /// AST binary-pattern segment list, threaded straight through
        /// to `compile_binary_pattern`.
        segments: Vec<BinarySegment>,
    },
    /// Bind a name to a value loaded through `source_ptr`. Side-effect
    /// only: emits `load` + `alloca` + `store` and inserts the binding
    /// into `Compiler.fn_state.variables` so subsequent
    /// [`IRInstruction::LoadLocal`] / [`IRInstruction::FieldChain`] calls
    /// can resolve it. Produces no SSA value (`dest` is absent).
    ///
    /// Slice 5b places these in `IRMatchArm.bind_instructions`, which
    /// the emitter runs at the top of the body block (after the cond
    /// branch fires) so bindings exist only when their pattern matched.
    /// The legacy `fn_state.variables` clone/restore around arm bodies
    /// retires as a result.
    PatternBindFromPtr {
        /// Source-level binding name.
        name: String,
        /// Resolved Expo type of the binding.
        ty: Type,
        /// Pointer-typed operand referencing the source storage
        /// ([`crate::lower::patterns::Lowerer::lower_pattern_to_instructions`]
        /// produces this either as the match's subject pointer, a
        /// projected variant-field alloca, or a union payload pointer).
        source_ptr: IROperand,
        /// Mirrors `ResolvedPattern::Bind { strict_llvm }`: when `true`,
        /// the codegen executor errors on unsupported types instead of
        /// falling back to `i8`. `TypedBinding` patterns set this; plain
        /// `Binding` patterns and field projections do not.
        strict_llvm: bool,
    },
    /// Compare a value loaded from `subject_ptr` (typed as
    /// `subject_ty`) to a literal constant. Produces `i1`.
    ///
    /// Reaches lowering via [`crate::lower::patterns::Lowerer::lower_pattern_to_instructions`]
    /// dispatching on `ResolvedPattern::LiteralEq`.
    PatternLiteralEq {
        /// SSA destination this instruction produces (the pattern's `i1`).
        dest: IRValueId,
        /// Pointer-typed operand referencing the subject's storage.
        subject_ptr: IROperand,
        /// Resolved Expo type of the subject (drives the load's LLVM type).
        subject_ty: Type,
        /// Literal to compare against (Bool / Int / Float / String).
        lit: ResolvedLiteral,
    },
    /// Project a single payload field out of an enum variant: GEP to
    /// the variant's payload, GEP to the field at `field_index`,
    /// `load_maybe_indirect` it, alloca, store. Produces a
    /// pointer-typed value: the new alloca, used as the subject
    /// pointer for a recursive sub-pattern test or as the source
    /// pointer for a [`IRInstruction::PatternBindFromPtr`].
    ///
    /// Reaches lowering via [`crate::lower::patterns::Lowerer::lower_pattern_to_instructions`]
    /// when walking the per-element/field structure of a
    /// `ResolvedPattern::EnumTuple` or `ResolvedPattern::EnumStruct`.
    /// The redundant payload GEP per field (rather than computing
    /// `payload_ptr` once and projecting many fields off of it) is
    /// intentional simplicity -- LLVM SROA / mem2reg coalesce them.
    PatternProjectVariantField {
        /// SSA destination this instruction produces (a pointer to the
        /// freshly-allocated field-value alloca).
        dest: IRValueId,
        /// Pointer-typed operand referencing the enum subject's storage.
        subject_ptr: IROperand,
        /// Resolved enum cache key (e.g. `"std.Option_$Int$"`).
        enum_key: String,
        /// Variant name (e.g. `"Some"`).
        variant: String,
        /// Index of the field within the variant's payload struct.
        field_index: u32,
        /// Resolved field type (drives `load_maybe_indirect` + alloca shape).
        field_ty: Type,
        /// Label hint for the emitted alloca / load.
        name_hint: String,
    },
    /// Project a single named field out of a plain (non-enum) struct: GEP
    /// directly into the struct at `field_index`, `load_maybe_indirect`
    /// it, alloca, store. Produces a pointer-typed value: the new
    /// alloca, used as the subject pointer for a recursive sub-pattern
    /// test or as the source pointer for a
    /// [`IRInstruction::PatternBindFromPtr`].
    ///
    /// Mirror of [`IRInstruction::PatternProjectVariantField`] minus the
    /// payload-pointer GEP step (a struct subject is already at the
    /// field-0 base, no tag/payload split). No payload-block gating is
    /// required either: a struct projection is unconditionally safe, so
    /// it lowers into the same open block as the other flat pattern
    /// primitives.
    PatternProjectStructField {
        /// SSA destination this instruction produces (a pointer to the
        /// freshly-allocated field-value alloca).
        dest: IRValueId,
        /// Pointer-typed operand referencing the struct subject's storage.
        subject_ptr: IROperand,
        /// Resolved struct cache key (e.g. `"std.Point"`, `"alpha.Pair_$Int$"`).
        struct_key: String,
        /// Index of the field within the struct.
        field_index: u32,
        /// Resolved field type (drives `load_maybe_indirect` + alloca shape).
        field_ty: Type,
        /// Label hint for the emitted alloca / load.
        name_hint: String,
    },
    /// Tag-equality check on an enum or union subject: load the i8 at
    /// the subject's tag slot (struct index 0) and compare against
    /// `tag`. Produces `i1`.
    ///
    /// Reaches lowering via [`crate::lower::patterns::Lowerer::lower_pattern_to_instructions`]
    /// for every `ResolvedPattern::EnumUnit` / `EnumTuple` / `EnumStruct`
    /// / `UnionMember`. `enum_key` is either an enum cache key or a
    /// union mangled name -- both expose the same tag-at-index-0
    /// LLVM struct shape.
    PatternTagEq {
        /// SSA destination this instruction produces (the pattern's `i1`).
        dest: IRValueId,
        /// Pointer-typed operand referencing the enum/union subject's storage.
        subject_ptr: IROperand,
        /// Enum cache key or union mangled name -- both look up the
        /// same `lookup_enum_struct_type` registry slot.
        enum_key: String,
        /// Expected tag value.
        tag: u8,
    },
    /// GEP into a union's payload field (struct index 1). Produces a
    /// pointer-typed value: the storage of the union's payload, used
    /// as the source pointer for a [`IRInstruction::PatternBindFromPtr`]
    /// in a `ResolvedPattern::UnionMember`. The caller knows the
    /// member's LLVM type from the static `Type::Union(members)`, so
    /// no per-member payload struct lookup is needed (mirrors the
    /// legacy `get_union_payload_ptr`).
    PatternUnionPayloadPtr {
        /// SSA destination this instruction produces (a pointer to the
        /// union's payload field).
        dest: IRValueId,
        /// Pointer-typed operand referencing the union subject's storage.
        subject_ptr: IROperand,
        /// Union mangled name (e.g. `"String_or_Int"`).
        union_mangled: String,
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
    /// Multi-segment field assignment (`a.b.c = v`). The codegen
    /// executor walks the GEP chain rooted at `base_name`'s storage
    /// pointer using `base_type` + `steps` (mirrors
    /// [`IRInstruction::FieldChain`]'s shape), then coerces the
    /// materialized `value` to `ty` and stores it into the resulting
    /// pointer. Side-effect only -- no SSA value is produced.
    ///
    /// Reaches lowering via [`crate::lower::statements`]'s
    /// [`expo_ast::ast::Statement::Assignment`] arm when the assign
    /// target is a multi-segment [`expo_ast::ast::LValue`]. Single-
    /// segment assigns lower to [`IRInstruction::StoreLocal`] instead.
    StoreField {
        /// Name of the chain's root binding.
        base_name: String,
        /// Resolved type of the root binding -- the first GEP step's
        /// struct layout source.
        base_type: Type,
        /// Successive field hops -- the executor GEPs through them in
        /// order on the root pointer to reach the final field slot.
        steps: Vec<ResolvedFieldStep>,
        /// Right-hand-side operand to materialize and store.
        value: IROperand,
        /// Resolved Expo type of the final field slot. Drives the
        /// numeric coercion applied to `value` before the store.
        ty: Type,
    },
    /// Single-segment local assignment / let-binding. When `is_decl`
    /// is `true` the executor allocates a fresh entry-block alloca,
    /// stores the materialized `value` (coerced to `ty`), and inserts
    /// the binding into `Compiler.fn_state.variables` with the
    /// supplied `ownership`. When `is_decl` is `false` the executor
    /// looks the binding up, coerces `value` to the binding's type,
    /// and stores into the existing slot. Side-effect only -- no
    /// SSA value is produced.
    ///
    /// Reaches lowering via [`crate::lower::statements`]'s
    /// [`expo_ast::ast::Statement::Assignment`] arm for single-
    /// segment [`expo_ast::ast::LValue`] / [`expo_ast::ast::Pattern`]
    /// targets. Multi-segment targets lower to
    /// [`IRInstruction::StoreField`] instead.
    StoreLocal {
        /// Source-level binding name.
        name: String,
        /// Right-hand-side operand to materialize and store.
        value: IROperand,
        /// Resolved Expo type of the binding. Drives the alloca's
        /// LLVM type when `is_decl`, and the coercion target either
        /// way.
        ty: Type,
        /// `true` for a fresh let-binding (alloca + insert), `false`
        /// for reassignment to an existing in-scope binding.
        is_decl: bool,
        /// Ownership classification for a freshly-bound value
        /// (`is_decl == true`). Computed at lowering time via
        /// [`crate::lower::ownership::ownership_for_expr`]. Ignored
        /// when `is_decl` is `false`.
        ownership: Option<Ownership>,
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
    /// Box a value of type `source_ty` into the surrounding tagged
    /// union `target_union`. Emission allocates a union-typed alloca,
    /// writes the discriminant tag (looked up via
    /// [`crate::lower::stmt::resolve_union_member`]) and the payload,
    /// and loads the union value back into `dest`.
    ///
    /// Reaches lowering via [`crate::lower::statements`] when an
    /// assignment / return value's span carries a
    /// [`expo_typecheck::context::Coercion::UnionWiden`] entry.
    /// Mirrors the legacy `apply_coercion` path for `UnionWiden`;
    /// other coercion variants do not exist today.
    UnionWrap {
        /// SSA destination this instruction produces (the boxed
        /// union value).
        dest: IRValueId,
        /// Operand carrying the source value to wrap.
        value: IROperand,
        /// Source type before wrapping (the union member type).
        source_ty: Type,
        /// Target union type. Drives the
        /// [`crate::lower::stmt::resolve_union_member`] lookup at
        /// emit time.
        target_union: Type,
    },
    /// Push an annotation-derived type-substitution scope onto
    /// `FnLowerState.type_subst` for the duration of the enclosing
    /// region. The matching [`IRInstruction::PopTypeSubst`] restores
    /// the prior state.
    ///
    /// Mirrors the codegen-side
    /// [`crate::compile_statement`] shim's annotation push so any
    /// transitional [`IRInstruction::Stub`] emitted between the push
    /// and pop sees the entries when its deferred `compile_expr`
    /// resolves a generic call (e.g. `List<Int>::new()`'s type-arg
    /// inference reads from `fn_lower.type_subst`). The shim push is
    /// scoped to a single top-level statement; this instruction
    /// extends the same scoping to body-block lifts where the
    /// statement's lowered instructions execute inside an enclosing
    /// construct's emit walker (e.g. an `if` body inside a top-level
    /// assignment).
    PushTypeSubst {
        /// Entries to insert into `fn_lower.type_subst`. Each entry
        /// shadows any pre-existing binding for the same name; the
        /// matching pop restores the prior values.
        entries: Vec<(String, Type)>,
    },
    /// Pop the most recent [`IRInstruction::PushTypeSubst`] scope:
    /// remove the names that were inserted and restore any prior
    /// values. The executor maintains a parallel stack of pre-push
    /// snapshots.
    PopTypeSubst {
        /// Names introduced by the matching push, in insertion
        /// order. Used by the executor to know which keys to remove
        /// from / restore on `fn_lower.type_subst`.
        names: Vec<String>,
    },
    /// Snapshot the current set of `fn_state.variables` bindings for
    /// the names a subsequent [`IRInstruction::ExitScope`] will
    /// restore. Used at match-arm boundaries (and similar block-local
    /// scopes) so per-arm pattern bindings don't leak forward.
    EnterScope,
    /// Restore the snapshot pushed by the matching
    /// [`IRInstruction::EnterScope`]: drop the named bindings from
    /// `fn_state.variables` and reinstate any pre-push entries the
    /// snapshot recorded.
    ExitScope {
        /// Names whose bindings should be removed from
        /// `fn_state.variables` (any pre-existing values for them are
        /// restored from the matching `EnterScope`'s snapshot).
        names: Vec<String>,
    },
    /// Placeholder for a `for binding in iterable ... end` loop. The
    /// elaboration pass ([`crate::elaborate::elaborate`]) rewrites
    /// each occurrence into a `length()` / `get()` / `Option`-unwrap /
    /// pattern-bind / `idx++` block sequence before codegen runs;
    /// codegen panics if it sees this variant.
    ForLoopStub {
        iterable: Box<Expr>,
        binding_pattern: Pattern,
        header_block: IRBlockId,
        body_entry: IRBlockId,
        exit_block: IRBlockId,
        elem_ty: Type,
        mangled_type: MonomorphizedTypeIdentifier,
    },
}

impl IRInstruction {
    /// SSA destination this instruction writes, or `None` for purely
    /// side-effecting instructions ([`IRInstruction::PatternBindFromPtr`]).
    /// Useful for emission walkers populating a `HashMap<IRValueId, _>`.
    pub fn dest(&self) -> Option<IRValueId> {
        match self {
            IRInstruction::BinaryOp { dest, .. }
            | IRInstruction::Call { dest, .. }
            | IRInstruction::FieldChain { dest, .. }
            | IRInstruction::FieldLoad { dest, .. }
            | IRInstruction::LoadConst { dest, .. }
            | IRInstruction::LoadLocal { dest, .. }
            | IRInstruction::MakeFnRef { dest, .. }
            | IRInstruction::MethodCall { dest, .. }
            | IRInstruction::PatternBinaryMatch { dest, .. }
            | IRInstruction::PatternLiteralEq { dest, .. }
            | IRInstruction::PatternProjectStructField { dest, .. }
            | IRInstruction::PatternProjectVariantField { dest, .. }
            | IRInstruction::PatternTagEq { dest, .. }
            | IRInstruction::PatternUnionPayloadPtr { dest, .. }
            | IRInstruction::Phi { dest, .. }
            | IRInstruction::Stub { dest, .. }
            | IRInstruction::UnaryOp { dest, .. }
            | IRInstruction::UnionWrap { dest, .. } => Some(*dest),
            IRInstruction::EnterScope
            | IRInstruction::ExitScope { .. }
            | IRInstruction::ForLoopStub { .. }
            | IRInstruction::PatternBindFromPtr { .. }
            | IRInstruction::PopTypeSubst { .. }
            | IRInstruction::PushTypeSubst { .. }
            | IRInstruction::StoreField { .. }
            | IRInstruction::StoreLocal { .. } => None,
        }
    }
}
