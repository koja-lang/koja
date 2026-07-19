//! Per-function lowering context: counters, [`CFGBuilder`], and the
//! `value -> IRType` index every recursive helper threads through.
//!
//! No language-aware logic lives here. This is the bookkeeping
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

use koja_ast::ast::Diagnostic;
use koja_ast::identifier::LocalId;

/// Snapshot of the live-slot map captured at a control-flow
/// construct's entry. Used by `match` / `cond` / `if` / `unless` /
/// ternary lowering to reset per-arm state and to merge post-arm
/// states into a joined post-construct state. See
/// [`FnLowerCtx::snapshot_slot_states`]. Maps each live
/// [`IRLocalId`] to the slot's [`IRType`] (pinned at its `LocalDecl`),
/// which the drop-glue emission consults.
pub(crate) type SlotStateSnapshot = BTreeMap<IRLocalId, IRType>;

/// One enclosing loop's `break` context, the exit block to branch to
/// and the subject-temp watermark at loop entry (see
/// [`FnLowerCtx::subject_temps_since`]).
#[derive(Clone, Copy)]
pub(crate) struct LoopExit {
    pub(crate) block: IRBlockId,
    pub(crate) subject_temp_watermark: usize,
}

use crate::cfg::CFGBuilder;
use crate::function::{IRBasicBlock, IRBlockId, IRFunction, IRSymbol};
use crate::generics::Instantiation;
use crate::local::IRLocalId;
use crate::types::{IRType, ValueId};

/// Per-package write-back bag threaded through every `lower_*`
/// helper. Bundling these sinks keeps helper signatures under the
/// clippy `too_many_arguments` threshold and makes the "what flows
/// back upward" group explicit. Read-only inputs (the typecheck
/// registry) stay separate args, since they have a different direction
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
    /// function's symbol. One wrapper per named fn used as a value.
    /// `synthesize_fn_as_closure_wrappers` reads the cache before
    /// minting to keep the package's function table dedup'd.
    pub(crate) fn_as_closure_wrappers: BTreeMap<IRSymbol, IRSymbol>,
    pub(crate) instantiations: Vec<Instantiation>,
    /// Dedupe set for [`crate::FunctionKind::SpawnWrapper`] symbols
    /// minted during `spawn` lowering. Each state cell gets one
    /// wrapper per [`super::process`] turn-around regardless of how
    /// many `spawn S.start(...)` sites hit it, so the IRPackage's
    /// function table only sees one entry.
    pub(crate) spawn_wrappers: BTreeSet<IRSymbol>,
    /// Closure bodies, fn-as-value adapters, and spawn-wrapper
    /// thunks minted during expression lowering. `lower_package`
    /// drains this and merges into [`crate::IRPackage::functions`].
    pub(crate) synthesized_functions: Vec<IRFunction>,
}

/// The shape every `lower_*` helper returns. `Open` carries the
/// trailing value (when the construct produces one) and the block
/// where flow continues. `Closed` signals that an inner statement
/// already terminated the function (the only path today is
/// `Statement::Return`). Closed branches don't fall through to a
/// surrounding merge block. The caller's wiring sees
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
/// `declared` and `locals` split the local-slot bookkeeping.
/// `declared` is monotonic. Presence means a `LocalDecl` was emitted
/// in the entry block, and it never shrinks, so a write after a
/// loop/branch boundary can't re-declare the slot. `locals` is the
/// path-sensitive live-slot map, with each entry carrying the slot's
/// [`IRType`] for drop-glue emission. Control-flow lowering
/// snapshots, restores, and merges it per arm.
///
/// One context per `IRFunction` (or per script body). Discarded after
/// the function's blocks are extracted via [`Self::into_blocks`], and
/// downstream consumers (seal, backends) build their own indices.
pub(crate) struct FnLowerCtx {
    pub(crate) cfg: CFGBuilder,
    next_value: u32,
    next_block: u32,
    value_types: BTreeMap<ValueId, IRType>,
    entry_block: Option<IRBlockId>,
    declared: BTreeSet<IRLocalId>,
    locals: BTreeMap<IRLocalId, IRType>,
    closures: ClosureState,
    /// Stack of pending loop exits, one entry per enclosing `loop` /
    /// `while`. Carries the exit block `break` branches to and the
    /// subject-temp watermark at loop entry, so `break` can release
    /// match subjects it escapes. [`super::loops`] pushes on entry
    /// and pops on exit.
    loop_exit: Vec<LoopExit>,
    /// SSA values that own a fresh heap allocation (and so may be
    /// moved into an owner or dropped as a temp). Marked at every
    /// producer that is *certain* to allocate fresh:
    ///
    /// - `Call` / `CallClosure` (callee returns an owned value),
    /// - `Concat` / `Clone`,
    /// - `StructInit` / `EnumConstruct` / `BinaryConstruct`,
    /// - a capturing `MakeClosure` (the heap env),
    /// - a heap-typed control-flow merge `BlockParam` (`if` / `cond` /
    ///   ternary / `match` / `receive`), whose arms hand it an acquired
    ///   value.
    ///
    /// Everything absent is treated as **borrowed** (a literal, `const`,
    /// slot/field read, or parameter), cloned on acquisition and never
    /// freed as a temp. Defaulting to borrowed keeps a misclassification
    /// leak-only, never a double-free. Each owned value has exactly one
    /// consumer: it is *moved* into an owner ([`super::ownership::materialize_owned`])
    /// or *released* at a use-and-release site ([`super::ownership::drop_discarded_temp`]).
    owned_values: BTreeSet<ValueId>,
    /// Slots that hold a borrowed reference rather than an owned
    /// value. Pattern binds write the subject's payload storage
    /// without a `Clone`, so no drop site may ever free these.
    /// Monotonic, like `declared`.
    borrowed_slots: BTreeSet<IRLocalId>,
    /// Owned match-subject temps whose arms are currently being
    /// lowered. A `return` or `break` inside an arm exits before the
    /// arm tail's subject release runs, so early-exit lowering drops
    /// this stack (innermost first) on the way out.
    /// [`super::match_expr`] pushes before lowering arms, pops after.
    pending_subject_temps: Vec<ValueId>,
}

/// Per-function closure bookkeeping. Two roles: outer fns mint
/// child names off `enclosing_symbol` + `next_index`, and
/// closure-body fns redirect outer-local idents through `captures`.
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
            "IR lower: closure expression encountered without an enclosing function symbol",
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
            declared: BTreeSet::new(),
            locals: BTreeMap::new(),
            closures: ClosureState::default(),
            loop_exit: Vec::new(),
            owned_values: BTreeSet::new(),
            borrowed_slots: BTreeSet::new(),
            pending_subject_temps: Vec::new(),
        }
    }

    /// Push an owned match-subject temp for the duration of its arms'
    /// lowering. Paired with [`Self::pop_subject_temp`].
    pub(crate) fn push_subject_temp(&mut self, value: ValueId) {
        self.pending_subject_temps.push(value);
    }

    /// Pop the innermost pending subject temp.
    pub(crate) fn pop_subject_temp(&mut self) {
        self.pending_subject_temps
            .pop()
            .expect("IR lower: pop_subject_temp on empty stack (push/pop imbalance)");
    }

    /// Pending subject temps beyond the `from` stack watermark,
    /// innermost first. `return` drains from 0 and `break` from the
    /// enclosing loop's entry watermark.
    pub(crate) fn subject_temps_since(&self, from: usize) -> Vec<ValueId> {
        self.pending_subject_temps[from..]
            .iter()
            .rev()
            .copied()
            .collect()
    }

    /// Current subject-temp stack depth, captured by loop lowering as
    /// the `break` drain watermark.
    pub(crate) fn subject_temp_watermark(&self) -> usize {
        self.pending_subject_temps.len()
    }

    /// Mark `local` as holding a borrowed reference (a pattern bind
    /// writing the match subject's payload storage). No drop site may
    /// free it. The subject's own release covers the storage.
    pub(crate) fn mark_slot_borrowed(&mut self, local: IRLocalId) {
        self.borrowed_slots.insert(local);
    }

    /// Does `local` hold a borrowed reference (see
    /// [`Self::mark_slot_borrowed`])?
    pub(crate) fn slot_is_borrowed(&self, local: IRLocalId) -> bool {
        self.borrowed_slots.contains(&local)
    }

    /// Mark `value` as owning a fresh heap allocation, eligible to be
    /// moved into an owner or freed as a discarded temp. Called by the
    /// drop-glue lowering at every certain-fresh producer.
    pub(crate) fn mark_owned(&mut self, value: ValueId) {
        self.owned_values.insert(value);
    }

    /// Does `value` own a fresh heap allocation? Absent values are
    /// borrowed (literal / `const` / read / param), cloned on
    /// acquisition, never freed as a temp.
    pub(crate) fn is_owned(&self, value: ValueId) -> bool {
        self.owned_values.contains(&value)
    }

    /// The owning heap-managed local slots live on this path, in
    /// reverse declaration order (LIFO drop). Used by the drop-glue
    /// lowering to free every owning slot at a control-flow exit.
    /// Borrowed slots (pattern binds) are excluded because their
    /// storage belongs to the match subject.
    pub(crate) fn heap_managed_slots(&self) -> Vec<(IRLocalId, IRType)> {
        let mut slots: Vec<(IRLocalId, IRType)> = self
            .locals
            .iter()
            .filter(|(local, ty)| ty.is_heap_managed() && !self.slot_is_borrowed(**local))
            .map(|(local, ty)| (*local, ty.clone()))
            .collect();
        slots.reverse();
        slots
    }

    /// The heap-managed local slots that became live since `snapshot`
    /// was captured, in reverse declaration order (LIFO drop). Loop
    /// lowering ([`super::loops`]) uses this to release body-scoped
    /// bindings at the end of each iteration: such bindings leave
    /// scope at the back-edge, so they must be dropped there and kept
    /// out of the function-exit drop set, where an unexecuted loop
    /// body would otherwise leave them uninitialized.
    pub(crate) fn heap_slots_declared_since(
        &self,
        snapshot: &SlotStateSnapshot,
    ) -> Vec<(IRLocalId, IRType)> {
        let mut slots: Vec<(IRLocalId, IRType)> = self
            .locals
            .iter()
            .filter(|(local, _)| !snapshot.contains_key(local))
            .filter(|(local, ty)| ty.is_heap_managed() && !self.slot_is_borrowed(**local))
            .map(|(local, ty)| (*local, ty.clone()))
            .collect();
        slots.reverse();
        slots
    }

    /// Push an enclosing loop's exit block, capturing the current
    /// subject-temp watermark. Paired with [`Self::pop_loop_exit`]
    /// by [`super::loops`].
    pub(crate) fn push_loop_exit(&mut self, exit: IRBlockId) {
        self.loop_exit.push(LoopExit {
            block: exit,
            subject_temp_watermark: self.subject_temp_watermark(),
        });
    }

    /// Pop the topmost loop-exit block. Panics on an empty stack,
    /// since every push has a matching pop in the same lowering scope.
    pub(crate) fn pop_loop_exit(&mut self) {
        self.loop_exit
            .pop()
            .expect("IR lower: pop_loop_exit on empty stack (push/pop imbalance)");
    }

    /// The innermost enclosing loop's exit, if any. `break` lowering
    /// consults this to pick its `Branch` target and the subject-temp
    /// drain watermark. `None` means `break` was reached outside any
    /// loop, which typecheck should have already diagnosed, so
    /// lowering panics.
    pub(crate) fn current_loop_exit(&self) -> Option<LoopExit> {
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

    /// Declare a control-flow merge `BlockParam` and mark it `owned`
    /// when the joined type is heap-managed. Every reaching arm hands
    /// the param an *acquired* value (see
    /// [`super::arms::finalize_arm_value`]), so the merged result is a
    /// single-owner temp the consumer moves into an owner or releases.
    /// Without it, a constructed/called arm value would leak through
    /// the join.
    pub(crate) fn declare_merge_param(&mut self, block: IRBlockId, ty: IRType) -> ValueId {
        let owned = ty.is_heap_managed();
        let dest = self.declare_block_param(block, ty);
        if owned {
            self.mark_owned(dest);
        }
        dest
    }

    /// The entry block of the function being lowered. Panics if
    /// called before [`Self::fresh_block`], since every consumer (param
    /// promotion, body-assignment lowering) sequences after entry
    /// creation.
    pub(crate) fn entry_block(&self) -> IRBlockId {
        self.entry_block.expect(
            "IR lower: entry_block consulted before any block was opened \
             (lower_function ordering bug)",
        )
    }

    /// Has `local` been declared yet in this function? `LocalWrite`s
    /// without a prior `LocalDecl` need to seed one in the entry
    /// block, and subsequent writes skip the seed. Monotonic. A
    /// slot stays declared even after a loop/branch boundary drops
    /// it from the live set, so it is never `LocalDecl`'d twice.
    pub(crate) fn local_is_declared(&self, local: IRLocalId) -> bool {
        self.declared.contains(&local)
    }

    /// Record that `local` has been declared with type `ty`. The
    /// caller should emit the `LocalDecl` when this is the first
    /// declaration. Subsequent calls are no-ops.
    pub(crate) fn mark_local_declared(&mut self, local: IRLocalId, ty: IRType) -> bool {
        if !self.declared.insert(local) {
            return false;
        }
        self.locals.insert(local, ty);
        true
    }

    /// Does `local` currently hold a value on this lowering path?
    /// False for a declared slot that fell out of the live set at a
    /// loop/branch boundary. Reassigning such a slot must skip both
    /// the re-decl and the stale-value drop.
    pub(crate) fn local_is_live(&self, local: IRLocalId) -> bool {
        self.locals.contains_key(&local)
    }

    /// Return a declared-but-dead slot to the live set so later
    /// drop-glue emission sees it again.
    pub(crate) fn mark_local_live(&mut self, local: IRLocalId, ty: IRType) {
        self.locals.insert(local, ty);
    }

    /// Clone the entire live-slot map. Control-flow
    /// lowering (`match`, `cond`, `if`/`else`, `unless`, ternary)
    /// captures this at the construct's entry so each arm can be
    /// lowered from a fresh baseline rather than inheriting the
    /// previous arm's post-state. Pairs with
    /// [`Self::restore_slot_states`] and [`Self::merge_slot_states`].
    pub(crate) fn snapshot_slot_states(&self) -> SlotStateSnapshot {
        self.locals.clone()
    }

    /// Reset the live-slot map to `snapshot`. Discards any per-arm
    /// slot state stamped on top of the entry-snapshot. The
    /// monotonic declared set is untouched.
    pub(crate) fn restore_slot_states(&mut self, snapshot: SlotStateSnapshot) {
        self.locals = snapshot;
    }

    /// Merge per-arm post-state snapshots into the live slot map.
    /// A slot survives the join only when every branch declared it,
    /// so declarations confined to one arm don't leak past the merge.
    pub(crate) fn merge_slot_states(&mut self, branches: Vec<SlotStateSnapshot>) {
        if branches.is_empty() {
            return;
        }
        let mut merged: BTreeMap<IRLocalId, IRType> = BTreeMap::new();
        let locals: BTreeSet<IRLocalId> = branches
            .iter()
            .flat_map(|snapshot| snapshot.keys().copied())
            .collect();
        for local in locals {
            if let Some(ty) = branches
                .iter()
                .map(|snapshot| snapshot.get(&local))
                .collect::<Option<Vec<_>>>()
                .and_then(|tys| tys.into_iter().next())
            {
                merged.insert(local, ty.clone());
            }
        }
        self.locals = merged;
    }

    /// Lookup the recorded `IRType` for `id`. Panics on a miss, since
    /// every emitted `ValueId` registers its type at allocation time,
    /// so a miss is a lowering bug.
    pub(crate) fn type_of(&self, id: ValueId) -> IRType {
        self.value_types
            .get(&id)
            .cloned()
            .unwrap_or_else(|| panic!("IR lower: missing type for {id} (lowering bug)"))
    }

    /// Consume the context and return the accumulated block list.
    /// Asserts via `CFGBuilder`'s closed-set that every block has had
    /// a real terminator stamped, since an unclosed block reaching the
    /// caller is a lowering bug.
    pub(crate) fn into_blocks(self) -> Vec<IRBasicBlock> {
        let (blocks, closed) = self.cfg.into_blocks_with_closed();
        for block in &blocks {
            if !closed.contains_key(&block.id) {
                panic!(
                    "IR lower: block {} ({}) was opened but never had its terminator set \
                     (lowering bug)",
                    block.id, block.label,
                );
            }
        }
        blocks
    }
}
