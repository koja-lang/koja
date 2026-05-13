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

use std::collections::{BTreeMap, BTreeSet};

use expo_ast::ast::Diagnostic;
use expo_ast::identifier::LocalId;

/// Snapshot of the per-slot [`SlotState`] map captured at a
/// control-flow construct's entry. Used by `match` / `cond` / `if`
/// / `unless` / ternary lowering to reset per-arm state and to
/// merge post-arm states into a joined post-construct state. See
/// [`FnLowerCtx::snapshot_slot_states`].
pub(crate) type SlotStateSnapshot = BTreeMap<IRLocalId, SlotState>;

use crate::cfg::CFGBuilder;
use crate::function::{IRBasicBlock, IRBlockId, IRFunction, IRSymbol};
use crate::generics::Instantiation;
use crate::local::IRLocalId;
use crate::ownership::Ownership;
use crate::types::{IRType, ValueId};

/// Per-package write-back bag threaded through every `lower_*`
/// helper. Bundling these sinks keeps helper signatures under the
/// clippy `too_many_arguments` threshold and makes the "what flows
/// back upward" group explicit. Read-only inputs (the typecheck
/// registry) stay separate args — they have a different direction
/// of flow and don't share lifetime scope.
///
/// `lower_program` / `lower_script` construct one [`LowerOutput`]
/// up front, thread `&mut output` through the per-package walks,
/// then destructure it: `diagnostics` short-circuits with
/// [`crate::error::LowerError::Diagnostics`] and `instantiations`
/// feeds [`crate::generics::instantiate`].
#[derive(Default)]
pub(crate) struct LowerOutput {
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// Cache of fn-as-value adapter wrappers, keyed by the wrapped
    /// function's symbol. One wrapper per named fn used as a value;
    /// `synthesize_fn_as_closure_wrappers` reads the cache before
    /// minting to keep the package's function table dedup'd.
    pub(crate) fn_as_closure_wrappers: BTreeMap<IRSymbol, IRSymbol>,
    pub(crate) instantiations: Vec<Instantiation>,
    /// Dedupe set for [`crate::FunctionKind::SpawnWrapper`] symbols
    /// minted during `spawn` lowering. Each state cell gets one
    /// wrapper per [`super::process`] turn-around regardless of how
    /// many `spawn S.start(...)` sites hit it; the IRPackage's
    /// function table only sees one entry.
    pub(crate) spawn_wrappers: BTreeSet<IRSymbol>,
    /// Closure bodies, fn-as-value adapters, and spawn-wrapper
    /// thunks minted during expression lowering. `lower_package`
    /// drains this and merges into [`crate::IRPackage::functions`].
    pub(crate) synthesized_functions: Vec<IRFunction>,
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
    /// Stack of pending loop-exit blocks — one entry per enclosing
    /// `loop` / `while`. [`super::loops`] pushes the exit on entry
    /// and pops on exit; [`super::body::lower_break_stmt`] peeks
    /// the top to find the [`IRBlockId`] its `Branch` should
    /// target. Mirrors v1's `FnLowerState::loop_exit` stack.
    loop_exit: Vec<IRBlockId>,
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
            loop_exit: Vec::new(),
        }
    }

    /// Push an enclosing loop's exit block. Paired with
    /// [`Self::pop_loop_exit`] by [`super::loops`].
    pub(crate) fn push_loop_exit(&mut self, exit: IRBlockId) {
        self.loop_exit.push(exit);
    }

    /// Pop the topmost loop-exit block. Panics on an empty stack —
    /// every push has a matching pop in the same lowering scope.
    pub(crate) fn pop_loop_exit(&mut self) {
        self.loop_exit
            .pop()
            .expect("alpha IR lower: pop_loop_exit on empty stack — push/pop imbalance");
    }

    /// The innermost enclosing loop's exit block, if any. `break`
    /// lowering consults this to pick its `Branch` target. `None`
    /// means `break` was reached outside any loop — typecheck
    /// should have already diagnosed; lowering panics.
    pub(crate) fn current_loop_exit(&self) -> Option<IRBlockId> {
        self.loop_exit.last().copied()
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

    /// Clone the entire `local -> SlotState` map. Control-flow
    /// lowering (`match`, `cond`, `if`/`else`, `unless`, ternary)
    /// captures this at the construct's entry so each arm can be
    /// lowered from a fresh baseline rather than inheriting the
    /// previous arm's post-state. Pairs with
    /// [`Self::restore_slot_states`] and [`Self::merge_slot_states`].
    pub(crate) fn snapshot_slot_states(&self) -> SlotStateSnapshot {
        self.locals.clone()
    }

    /// Reset the slot-state map to `snapshot`. Discards any
    /// per-arm mutations stamped on top of the entry-snapshot.
    pub(crate) fn restore_slot_states(&mut self, snapshot: SlotStateSnapshot) {
        self.locals = snapshot;
    }

    /// Merge per-arm post-state snapshots into the live slot map.
    /// Conservative join: a slot's merged `ownership` adopts the
    /// per-arm stamp only when every branch agreed on it, else
    /// falls back to [`Ownership::Unowned`]; `moved` is the AND
    /// across branches (only carry the moved flag through when
    /// every branch consumed the slot).
    ///
    /// This avoids both failure modes of the previous flat
    /// tracking: an over-promoted `Owned` would synthesize a
    /// drop on an Unowned literal at function exit (SIGABRT),
    /// while an under-promoted `Unowned` would skip a needed drop
    /// when the slot definitely holds heap storage. Slots present
    /// only in some branches stay absent in the merged map; new
    /// declarations inside one arm don't leak past the join.
    pub(crate) fn merge_slot_states(&mut self, branches: Vec<SlotStateSnapshot>) {
        if branches.is_empty() {
            return;
        }
        let mut merged: BTreeMap<IRLocalId, SlotState> = BTreeMap::new();
        let locals: BTreeSet<IRLocalId> = branches
            .iter()
            .flat_map(|snapshot| snapshot.keys().copied())
            .collect();
        for local in locals {
            let mut per_arm: Vec<&SlotState> = Vec::with_capacity(branches.len());
            let mut present_in_all = true;
            for snapshot in &branches {
                match snapshot.get(&local) {
                    Some(state) => per_arm.push(state),
                    None => {
                        present_in_all = false;
                        break;
                    }
                }
            }
            if !present_in_all {
                continue;
            }
            let first = per_arm[0];
            let ownership = if per_arm
                .iter()
                .all(|state| state.ownership == first.ownership)
            {
                first.ownership
            } else {
                Ownership::Unowned
            };
            let moved = per_arm.iter().all(|state| state.moved);
            merged.insert(
                local,
                SlotState {
                    moved,
                    ownership,
                    ty: first.ty.clone(),
                },
            );
        }
        self.locals = merged;
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
