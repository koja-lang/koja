//! Per-function lowering context: counters, [`CFGBuilder`], and the
//! `value -> IRType` index every recursive helper threads through.
//!
//! No language-aware logic lives here — this is the bookkeeping
//! layer the rest of the [`crate::lower`] modules sit on top of.
//!
//! Three types live here together because they're co-evolving and
//! never used independently:
//!
//! - [`FnLowerCtx`] owns per-function mutable scratch state (the
//!   CFG, value/block counters, the per-function `local` set).
//! - [`LowerOutput`] is the per-package write-back bag every helper
//!   threads through: feature-gap diagnostics flowing back to
//!   `lower_program` / `lower_script`, plus the discovered
//!   generic-instantiation set the [`crate::generics`] driver
//!   consumes after lowering finishes.
//! - [`FlowResult`] is the return shape every `lower_*` helper
//!   produces, distinguishing "flow continues at this block with
//!   this value" from "flow terminated already (e.g. via early
//!   `return`)".

use std::collections::BTreeMap;

use expo_alpha_typecheck::Coercions;
use expo_ast::ast::Diagnostic;
use expo_ast::identifier::LocalId;

use crate::cfg::CFGBuilder;
use crate::function::{IRBasicBlock, IRBlockId, IRFunction, IRSymbol};
use crate::generics::Instantiation;
use crate::local::IRLocalId;
use crate::ownership::Ownership;
use crate::types::{IRType, ValueId};

/// Per-package write-back bag threaded through every `lower_*`
/// helper. Bundling these two sinks keeps helper signatures under
/// the clippy `too_many_arguments` threshold and makes the
/// "what flows back upward" group explicit. Read-only inputs
/// (the typecheck registry) stay separate args — they have a
/// different direction of flow and don't share lifetime scope.
///
/// `lower_program` / `lower_script` construct one [`LowerOutput`]
/// up front (seeding `coercions` from the typecheck output), thread
/// `&mut output` through the per-package walks, then destructure
/// it: `diagnostics` short-circuits with
/// [`crate::error::LowerError::Diagnostics`] and `instantiations`
/// feeds [`crate::generics::instantiate`].
///
/// `coercions` is a clone of `expo_alpha_typecheck::CheckedProgram::coercions`
/// — the program-wide span-keyed numeric-literal width sink the
/// expression / constant lowering helpers consult to mint
/// `ConstValue::Int*` / `ConstValue::Float*` at the typecheck-
/// recorded target width instead of the default 64-bit form. Read
/// here, never written; clone (rather than borrow) sidesteps the
/// otherwise-required lifetime parameter on `LowerOutput`.
#[derive(Default)]
pub(crate) struct LowerOutput {
    pub(crate) coercions: Coercions,
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// Cache of fn-as-value adapter wrappers, keyed by the wrapped
    /// function's symbol. One wrapper per named fn used as a value;
    /// `synthesize_fn_as_closure_wrappers` reads the cache before
    /// minting to keep the package's function table dedup'd.
    pub(crate) fn_as_closure_wrappers: BTreeMap<IRSymbol, IRSymbol>,
    pub(crate) instantiations: Vec<Instantiation>,
    /// Closure bodies and fn-as-value adapters minted during
    /// expression lowering. `lower_package` drains this and merges
    /// into [`crate::IRPackage::functions`].
    pub(crate) synthesized_functions: Vec<IRFunction>,
}

impl LowerOutput {
    /// Construct a [`LowerOutput`] seeded with the typecheck's
    /// numeric-literal coercion table. Use over `Self::default()`
    /// at the entry points (`lower_program` / `lower_script`); the
    /// monomorphization driver and other in-flight consumers stick
    /// with the default constructor since they don't see literals
    /// outside the seeded entry walk.
    pub(crate) fn with_coercions(coercions: Coercions) -> Self {
        Self {
            coercions,
            ..Self::default()
        }
    }
}

/// The shape every `lower_*` helper returns. `Open` carries the
/// trailing value (when the construct produces one) and the block
/// where flow continues; `Closed` signals that an inner statement
/// already terminated the function (the only path today is
/// `Statement::Return`). Closed branches don't fall through to a
/// surrounding merge block — the caller's wiring sees
/// `FlowResult::Closed` directly and skips the fall-through wiring
/// it would otherwise emit.
#[derive(Debug, Clone)]
pub(crate) enum FlowResult {
    Open {
        value: Option<ValueId>,
        block: IRBlockId,
    },
    Closed,
}

/// Per-slot bookkeeping for the lowering layer's drop pipeline. One
/// entry per [`IRLocalId`] declared in the function. Records:
///
/// - `moved` — `true` once a [`crate::IRInstruction::MoveOutLocal`]
///   has consumed the slot. Drop emission skips moved slots: the
///   value transferred to the new owner.
/// - `ownership` — the most-recent [`crate::IRInstruction::LocalWrite`]'s
///   stamp. Drop emission keys on this to decide whether the slot's
///   storage is heap-allocated.
/// - `ty` — the slot's [`IRType`], pinned at the [`crate::IRInstruction::LocalDecl`]
///   site. Drop emission threads this back into
///   [`crate::IRInstruction::DropLocal::ty`] so the LLVM backend
///   can dispatch the correct `free` shape without walking the
///   function's instruction list.
///
/// The seal pass already enforces that locals are never read across
/// CFG joins (one `LocalWrite` per slot per branch path); this slot
/// state is a function-flat snapshot consulted at fn-exit drop
/// emission, not a per-block lattice.
#[derive(Clone, Debug)]
pub(crate) struct SlotState {
    pub(crate) moved: bool,
    pub(crate) ownership: Ownership,
    pub(crate) ty: IRType,
}

/// Per-function lowering context. Owns the [`CFGBuilder`] plus the
/// `ValueId` / `IRBlockId` counters and a `value -> IRType` index
/// callers consult to derive operator result types and the function's
/// return type without re-querying the typecheck registry.
///
/// `entry_block` is set as soon as the function's first block is
/// created (before parameter promotion), so any later body lowering
/// step can append [`crate::function::IRInstruction::LocalDecl`]s
/// into the entry regardless of the currently-open block.
///
/// `locals` tracks the per-slot [`SlotState`] (ownership stamp +
/// moved flag) that drop emission keys on at fn exit. The map is
/// also the canonical "declared local" set: presence in the map
/// means a `LocalDecl` was emitted in the entry block.
///
/// `value_sources` is the inverse `ValueId -> IRLocalId` index for
/// the most-recent [`crate::IRInstruction::LocalRead`] of each
/// `value`. The return-path lowerer consults it to decide whether a
/// returned `ValueId` originates from a local slot (eligible for
/// `MoveOutLocal` substitution) or is a direct expression result
/// (no slot to consume).
///
/// One context per `IRFunction` (or per script body). Discarded after
/// the function's blocks are extracted via [`Self::into_blocks`];
/// downstream consumers (seal, backends) build their own indices.
pub(crate) struct FnLowerCtx {
    pub(crate) cfg: CFGBuilder,
    next_value: u32,
    next_block: u32,
    value_types: BTreeMap<ValueId, IRType>,
    entry_block: Option<IRBlockId>,
    locals: BTreeMap<IRLocalId, SlotState>,
    value_sources: BTreeMap<ValueId, IRLocalId>,
    closures: ClosureState,
}

/// Per-function closure bookkeeping. Two roles: outer fns mint
/// child names off `enclosing_symbol` + `next_index`; closure-body
/// fns redirect outer-local idents through `captures`.
#[derive(Default)]
pub(crate) struct ClosureState {
    enclosing_symbol: Option<IRSymbol>,
    next_index: u32,
    captures: BTreeMap<LocalId, u32>,
}

impl ClosureState {
    /// Seed the enclosing fn's mangled symbol so child closure
    /// bodies can derive `__closure<N>` names off it.
    pub(crate) fn set_enclosing_symbol(&mut self, symbol: IRSymbol) {
        self.enclosing_symbol = Some(symbol);
    }

    /// Mint the next `<enclosing>__closure<N>` symbol. Panics when
    /// no enclosing symbol was seeded.
    pub(crate) fn mint_symbol(&mut self) -> IRSymbol {
        let parent = self.enclosing_symbol.as_ref().expect(
            "alpha IR lower: closure expression encountered without an enclosing function symbol",
        );
        let suffix = format!("__closure{}", self.next_index);
        self.next_index += 1;
        parent.derived(&suffix)
    }

    /// Record the captures-by-position list for a closure-body ctx.
    pub(crate) fn set_captures(&mut self, captures: &[LocalId]) {
        for (index, local) in captures.iter().enumerate() {
            self.captures.insert(*local, index as u32);
        }
    }

    /// `Some(index)` if `local` is a captured outer-scope local
    /// inside this body. Ident lowering consults this before
    /// falling back to a `LocalRead`.
    pub(crate) fn capture_index(&self, local: LocalId) -> Option<u32> {
        self.captures.get(&local).copied()
    }
}

impl FnLowerCtx {
    pub(crate) fn new() -> Self {
        Self {
            cfg: CFGBuilder::new(),
            next_value: 0,
            next_block: 0,
            value_types: BTreeMap::new(),
            entry_block: None,
            locals: BTreeMap::new(),
            value_sources: BTreeMap::new(),
            closures: ClosureState::default(),
        }
    }

    pub(crate) fn closures_mut(&mut self) -> &mut ClosureState {
        &mut self.closures
    }

    pub(crate) fn closures(&self) -> &ClosureState {
        &self.closures
    }

    /// Mint a fresh `ValueId` and record its `IRType`.
    pub(crate) fn fresh_value(&mut self, ty: IRType) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        self.value_types.insert(id, ty);
        id
    }

    /// Mint a fresh `IRBlockId` and add the corresponding empty
    /// block to the [`CFGBuilder`]. The first block created against
    /// this context is recorded as [`Self::entry_block`] so later
    /// body lowering can append `LocalDecl`s back into the entry
    /// regardless of the currently-open block.
    pub(crate) fn fresh_block(&mut self, label: impl Into<String>) -> IRBlockId {
        let id = IRBlockId(self.next_block);
        self.next_block += 1;
        self.cfg.add_block(id, label);
        if self.entry_block.is_none() {
            self.entry_block = Some(id);
        }
        id
    }

    /// Declare a typed [`crate::function::BlockParam`] on `block` and
    /// return the fresh `ValueId` that names it. The minted id is
    /// registered in the value-types index just like every other
    /// `ValueId` from [`Self::fresh_value`], so downstream operand
    /// lookups (and the seal pass) see a consistent type for it.
    /// Used by value-producing control-flow lowering (`if`/`else`,
    /// `cond`) to declare merge-block result params.
    pub(crate) fn declare_block_param(&mut self, block: IRBlockId, ty: IRType) -> ValueId {
        let dest = self.fresh_value(ty.clone());
        self.cfg.declare_block_param(block, dest, ty);
        dest
    }

    /// The entry block of the function being lowered. Panics if
    /// called before [`Self::fresh_block`] — every consumer (param
    /// promotion, body-assignment lowering) sequences after entry
    /// creation.
    pub(crate) fn entry_block(&self) -> IRBlockId {
        self.entry_block.expect(
            "alpha IR lower: entry_block consulted before any block was opened — \
             lower_function ordering bug",
        )
    }

    /// Has `local` been declared yet in this function? `LocalWrite`s
    /// without a prior `LocalDecl` need to seed one in the entry
    /// block; subsequent writes skip the seed.
    pub(crate) fn local_is_declared(&self, local: IRLocalId) -> bool {
        self.locals.contains_key(&local)
    }

    /// Record that `local` has been declared with type `ty`. The
    /// caller should emit the `LocalDecl` when this is the first
    /// declaration; subsequent calls are no-ops on the slot-state
    /// side. The slot starts with [`Ownership::Unowned`] /
    /// `moved = false`; the matching [`Self::mark_local_written`]
    /// from the parameter-promotion or first-assignment site
    /// supplies the real ownership stamp.
    pub(crate) fn mark_local_declared(&mut self, local: IRLocalId, ty: IRType) -> bool {
        if self.locals.contains_key(&local) {
            return false;
        }
        self.locals.insert(
            local,
            SlotState {
                moved: false,
                ownership: Ownership::Unowned,
                ty,
            },
        );
        true
    }

    /// Snapshot of `local`'s current [`SlotState`]. Returns `None`
    /// when the slot was never declared; consumers (drop emission,
    /// reassignment-drop check) treat absence as "no slot to consider"
    /// rather than panicking, since seal validates declaration
    /// invariants in a separate pass.
    pub(crate) fn slot_state(&self, local: IRLocalId) -> Option<&SlotState> {
        self.locals.get(&local)
    }

    /// Update `local`'s slot state to reflect a fresh
    /// [`crate::IRInstruction::LocalWrite`] with the given
    /// `ownership`. Resets `moved` (the slot is live again), and
    /// stamps the new ownership over any previous one — per-write
    /// stamping matches v1's `StoreLocal` semantics, where a slot's
    /// ownership can change as different RHS expressions assign to
    /// it (e.g. `s = "literal"` then `s = a <> b`). Panics when the
    /// slot was never declared; the lowering layer always emits a
    /// `LocalDecl` before any `LocalWrite`.
    pub(crate) fn mark_local_written(&mut self, local: IRLocalId, ownership: Ownership) {
        let state = self.locals.get_mut(&local).unwrap_or_else(|| {
            panic!(
                "alpha IR lower: mark_local_written for undeclared slot `{local}` — lowering bug"
            )
        });
        state.moved = false;
        state.ownership = ownership;
    }

    /// Mark `local` consumed by a [`crate::IRInstruction::MoveOutLocal`].
    /// Drop emission skips moved slots — the value transferred to a
    /// new owner (today: the function return; future: cross-local
    /// moves). No-op when the slot was never declared (defensive;
    /// seal catches stray references).
    pub(crate) fn mark_local_moved(&mut self, local: IRLocalId) {
        if let Some(state) = self.locals.get_mut(&local) {
            state.moved = true;
        }
    }

    /// Iterator over every Live & Owned slot, paired with its
    /// [`IRType`], in declaration order. "Live" = `!moved`; "Owned"
    /// = `ownership == Ownership::Owned`. The drop-emission helper
    /// ([`super::drops::emit_function_exit_drops`]) consumes this
    /// to decide which slots need a `DropLocal` before the
    /// function-exit terminator.
    pub(crate) fn live_owned_locals(&self) -> impl Iterator<Item = (IRLocalId, IRType)> + '_ {
        self.locals
            .iter()
            .filter(|(_, state)| !state.moved && matches!(state.ownership, Ownership::Owned))
            .map(|(local, state)| (*local, state.ty.clone()))
    }

    /// Record that the [`ValueId`] `value` was just minted by a
    /// [`crate::IRInstruction::LocalRead`] of slot `local`. Used by
    /// the return-path lowerer to detect when a returned value
    /// originated from a local slot (eligible for `MoveOutLocal`
    /// substitution) versus an expression intermediate.
    pub(crate) fn record_value_source(&mut self, value: ValueId, local: IRLocalId) {
        self.value_sources.insert(value, local);
    }

    /// Reverse-lookup the [`IRLocalId`] that minted `value` via a
    /// recorded [`crate::IRInstruction::LocalRead`]. Returns `None`
    /// for values from any other source (constants, op results,
    /// calls, etc.).
    pub(crate) fn value_source(&self, value: ValueId) -> Option<IRLocalId> {
        self.value_sources.get(&value).copied()
    }

    /// Lookup the recorded `IRType` for `id`. Panics on a miss —
    /// every emitted `ValueId` registers its type at allocation time,
    /// so a miss is a lowering bug.
    pub(crate) fn type_of(&self, id: ValueId) -> IRType {
        self.value_types
            .get(&id)
            .cloned()
            .unwrap_or_else(|| panic!("alpha IR lower: missing type for {id} — lowering bug"))
    }

    /// Consume the context and return the accumulated block list.
    /// Asserts via `CFGBuilder`'s closed-set that every block has had
    /// a real terminator stamped — an unclosed block reaching the
    /// caller is a lowering bug.
    pub(crate) fn into_blocks(self) -> Vec<IRBasicBlock> {
        let (blocks, closed) = self.cfg.into_blocks_with_closed();
        for block in &blocks {
            if !closed.contains_key(&block.id) {
                panic!(
                    "alpha IR lower: block {} ({}) was opened but never had its terminator set — \
                     lowering bug",
                    block.id, block.label,
                );
            }
        }
        blocks
    }
}
