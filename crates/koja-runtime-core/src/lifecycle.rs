//! The per-slot lifecycle word: generation, [`ProcessState`], and the
//! `on_cpu` claim flag packed into one `AtomicU64`, so every lifecycle
//! edge is a single CAS (see the edge table in
//! `design/SCHEDULER-PROTOCOL.md`).
//!
//! The generation lives in the word because a CAS on state alone is
//! ABA-unsafe: between a claimer's generation check and its CAS the slot
//! could be freed and respawned, and the CAS would claim the new
//! occupant while the claimer proceeds with the old PID. With the
//! generation in the word, a stale edge fails the CAS itself.
//!
//! Ordering: a successful claim is an acquire and [`LifecycleWord::release`]
//! (the `on_cpu` clear) is a release, forming the happens-before pair
//! that licenses a claimer's read of execution state the previous owner
//! wrote. Every other edge uses acquire/release too; none is hot enough
//! for relaxed to matter.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::process_table::ProcessState;

/// Bit layout: `[63:32]` generation, `[3]` on_cpu, `[2:0]` state code.
const ON_CPU_BIT: u64 = 1 << 3;
const STATE_MASK: u64 = 0b111;
const GENERATION_SHIFT: u32 = 32;

/// State code for a vacant slot. [`ProcessState`] codes start at 1.
const VACANT: u64 = 0;

fn state_code(state: ProcessState) -> u64 {
    match state {
        ProcessState::Created => 1,
        ProcessState::Runnable => 2,
        ProcessState::Running => 3,
        ProcessState::Blocked => 4,
        ProcessState::WaitingIO => 5,
        ProcessState::Dead => 6,
    }
}

fn state_of(code: u64) -> Option<ProcessState> {
    match code {
        1 => Some(ProcessState::Created),
        2 => Some(ProcessState::Runnable),
        3 => Some(ProcessState::Running),
        4 => Some(ProcessState::Blocked),
        5 => Some(ProcessState::WaitingIO),
        6 => Some(ProcessState::Dead),
        _ => None,
    }
}

const fn pack(generation: u32, state: u64, on_cpu: bool) -> u64 {
    ((generation as u64) << GENERATION_SHIFT) | if on_cpu { ON_CPU_BIT } else { 0 } | state
}

/// One decoded observation of a [`LifecycleWord`]. `state` is `None` for
/// a vacant slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WordState {
    pub generation: u32,
    pub on_cpu: bool,
    pub state: Option<ProcessState>,
}

impl WordState {
    fn decode(word: u64) -> Self {
        Self {
            generation: (word >> GENERATION_SHIFT) as u32,
            on_cpu: word & ON_CPU_BIT != 0,
            state: state_of(word & STATE_MASK),
        }
    }

    /// Whether this observation is the live (non-`Dead`) occupant for
    /// `generation`.
    pub fn is_alive(&self, generation: u32) -> bool {
        self.generation == generation && self.state.is_some_and(|state| state != ProcessState::Dead)
    }
}

/// The packed atomic word. One per slot, alongside (not inside) the
/// slot's mutex: coupled edges CAS it while holding that mutex, the
/// claim family CASes it lock-free.
pub(crate) struct LifecycleWord(AtomicU64);

impl LifecycleWord {
    /// A vacant slot at generation 0 (no PID ever resolves to it: live
    /// generations start at 1).
    pub const fn new() -> Self {
        Self(AtomicU64::new(pack(0, VACANT, false)))
    }

    pub fn load(&self) -> WordState {
        WordState::decode(self.0.load(Ordering::Acquire))
    }

    /// One CAS attempt of `current -> next`, both re-encoded from decoded
    /// observations. Failure returns the fresh observation.
    fn try_swap(&self, current: WordState, next: WordState) -> Result<(), WordState> {
        let encode = |lifecycle: WordState| {
            pack(
                lifecycle.generation,
                lifecycle.state.map_or(VACANT, state_code),
                lifecycle.on_cpu,
            )
        };
        self.0
            .compare_exchange(
                encode(current),
                encode(next),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(drop)
            .map_err(WordState::decode)
    }

    /// The claim edge: `Created`/`Runnable`, off-cpu, at `generation` ->
    /// `Running`, on-cpu. `false` for a stale entry (wrong generation,
    /// already claimed, killed, or not runnable). The success acquire
    /// licenses the claimer's read of the owner-published execution state.
    pub fn try_claim(&self, generation: u32) -> bool {
        let mut observed = self.load();
        loop {
            let claimable = observed.generation == generation
                && !observed.on_cpu
                && matches!(
                    observed.state,
                    Some(ProcessState::Created | ProcessState::Runnable)
                );
            if !claimable {
                return false;
            }
            let claimed = WordState {
                on_cpu: true,
                state: Some(ProcessState::Running),
                ..observed
            };
            match self.try_swap(observed, claimed) {
                Ok(()) => return true,
                Err(fresh) => observed = fresh,
            }
        }
    }

    /// A guarded state edge at `generation`: `from -> to`, leaving
    /// `on_cpu` as is. `false` when the observed state is not `from`
    /// (the caller counts the stale skip). Callers hold the slot mutex
    /// for every edge coupled to messaging state (park, wake, kill).
    pub fn try_edge(&self, generation: u32, from: ProcessState, to: ProcessState) -> bool {
        debug_assert!(
            crate::process_table::is_legal_transition(from, to),
            "illegal lifecycle edge requested: {from:?} -> {to:?}",
        );
        let mut observed = self.load();
        loop {
            if observed.generation != generation || observed.state != Some(from) {
                return false;
            }
            let next = WordState {
                state: Some(to),
                ..observed
            };
            match self.try_swap(observed, next) {
                Ok(()) => return true,
                Err(fresh) => observed = fresh,
            }
        }
    }

    /// The kill edge: any live state at `generation` -> `Dead`, leaving
    /// `on_cpu` as is. Returns the prior observation so the killer can
    /// tell an off-cpu target (reclaim now) from an on-cpu one (mark
    /// only, the owner reclaims at switch-out), or `None` when the
    /// target is already dead, vacant, or stale.
    pub fn try_kill(&self, generation: u32) -> Option<WordState> {
        let mut observed = self.load();
        loop {
            if !observed.is_alive(generation) {
                return None;
            }
            let dead = WordState {
                state: Some(ProcessState::Dead),
                ..observed
            };
            match self.try_swap(observed, dead) {
                Ok(()) => return Some(observed),
                Err(fresh) => observed = fresh,
            }
        }
    }

    /// The switch-out release: clears `on_cpu` at `generation`, returning
    /// the state observed at the clear so the owner routes the process
    /// (re-enqueue a `Runnable`, reclaim a `Dead`). The release publishes
    /// the owner's execution-state writes to the next claimer. `None`
    /// when the slot is stale (the owner already released and the slot
    /// moved on, which no correct driver produces).
    pub fn release(&self, generation: u32) -> Option<ProcessState> {
        let mut observed = self.load();
        loop {
            if observed.generation != generation || !observed.on_cpu {
                return None;
            }
            let released = WordState {
                on_cpu: false,
                ..observed
            };
            match self.try_swap(observed, released) {
                Ok(()) => return observed.state,
                Err(fresh) => observed = fresh,
            }
        }
    }

    /// Occupies a vacant slot at `generation` as `Created`. Called under
    /// the registry mutex (which owns the freelist), so the slot cannot
    /// be double-occupied; the store still asserts vacancy in debug.
    pub fn occupy(&self, generation: u32) {
        let prior = self.0.swap(
            pack(generation, state_code(ProcessState::Created), false),
            Ordering::AcqRel,
        );
        debug_assert_eq!(
            WordState::decode(prior).state,
            None,
            "occupying a non-vacant slot",
        );
    }

    /// Vacates the slot, bumping the generation so every stale edge aimed
    /// at the old occupant fails its CAS. Called with the slot mutex held
    /// (a mutex holder that validated the generation must not have the
    /// slot recycled under it). Returns the next spawn's generation.
    pub fn vacate(&self) -> u32 {
        let observed = self.load();
        debug_assert_eq!(
            observed.state,
            Some(ProcessState::Dead),
            "vacating a slot whose occupant is not dead",
        );
        let next_generation = observed.generation.wrapping_add(1);
        self.0
            .store(pack(next_generation, VACANT, false), Ordering::Release);
        next_generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word occupied at `generation`, in `state`, optionally claimed.
    fn word(generation: u32, state: ProcessState, on_cpu: bool) -> LifecycleWord {
        let word = LifecycleWord::new();
        word.0.store(
            pack(generation, state_code(state), on_cpu),
            Ordering::Relaxed,
        );
        word
    }

    #[test]
    fn pack_roundtrips_every_state_and_flag() {
        for state in [
            ProcessState::Created,
            ProcessState::Runnable,
            ProcessState::Running,
            ProcessState::Blocked,
            ProcessState::WaitingIO,
            ProcessState::Dead,
        ] {
            for on_cpu in [false, true] {
                for generation in [1u32, 7, u32::MAX] {
                    let decoded = WordState::decode(pack(generation, state_code(state), on_cpu));
                    assert_eq!(decoded.generation, generation);
                    assert_eq!(decoded.on_cpu, on_cpu);
                    assert_eq!(decoded.state, Some(state));
                }
            }
        }
    }

    #[test]
    fn claim_takes_a_runnable_and_rejects_everything_else() {
        let runnable = word(3, ProcessState::Runnable, false);
        assert!(runnable.try_claim(3));
        let claimed = runnable.load();
        assert_eq!(claimed.state, Some(ProcessState::Running));
        assert!(claimed.on_cpu);
        assert!(!runnable.try_claim(3), "already claimed");

        assert!(word(3, ProcessState::Created, false).try_claim(3));
        assert!(
            !word(3, ProcessState::Runnable, false).try_claim(2),
            "stale generation"
        );
        assert!(
            !word(3, ProcessState::Runnable, true).try_claim(3),
            "still being saved"
        );
        assert!(!word(3, ProcessState::Dead, false).try_claim(3), "killed");
        assert!(
            !word(3, ProcessState::Blocked, false).try_claim(3),
            "parked"
        );
        assert!(!LifecycleWord::new().try_claim(0), "vacant");
    }

    #[test]
    fn edges_apply_only_from_their_expected_state() {
        let running = word(1, ProcessState::Running, true);
        assert!(running.try_edge(1, ProcessState::Running, ProcessState::Blocked));
        assert_eq!(running.load().state, Some(ProcessState::Blocked));
        assert!(running.load().on_cpu, "edge leaves the claim flag alone");

        assert!(
            !running.try_edge(1, ProcessState::Running, ProcessState::Blocked),
            "no longer Running",
        );
        assert!(
            !running.try_edge(2, ProcessState::Blocked, ProcessState::Runnable),
            "stale generation",
        );
        assert!(running.try_edge(1, ProcessState::Blocked, ProcessState::Runnable));
    }

    #[test]
    fn kill_reports_the_prior_observation_once() {
        let on_cpu = word(5, ProcessState::Running, true);
        let prior = on_cpu.try_kill(5).expect("live target");
        assert_eq!(prior.state, Some(ProcessState::Running));
        assert!(prior.on_cpu, "killer sees the claim, so it defers reclaim");
        assert_eq!(on_cpu.load().state, Some(ProcessState::Dead));
        assert!(on_cpu.try_kill(5).is_none(), "second kill is a no-op");

        let parked = word(5, ProcessState::Blocked, false);
        let prior = parked.try_kill(5).expect("live target");
        assert!(!prior.on_cpu, "off-cpu target reclaims inline");
        assert!(parked.try_kill(4).is_none(), "stale generation");
        assert!(LifecycleWord::new().try_kill(0).is_none(), "vacant");
    }

    #[test]
    fn release_reports_the_state_at_switch_out() {
        let yielded = word(2, ProcessState::Runnable, true);
        assert_eq!(yielded.release(2), Some(ProcessState::Runnable));
        assert!(!yielded.load().on_cpu);
        assert_eq!(yielded.release(2), None, "already released");

        // A kill that landed mid-run: the owner observes Dead at release
        // and reclaims.
        let killed = word(2, ProcessState::Running, true);
        killed.try_kill(2);
        assert_eq!(killed.release(2), Some(ProcessState::Dead));
    }

    #[test]
    fn vacate_bumps_the_generation_and_strands_stale_edges() {
        let dead = word(9, ProcessState::Dead, false);
        assert_eq!(dead.vacate(), 10);
        let vacant = dead.load();
        assert_eq!(vacant.state, None);
        assert_eq!(vacant.generation, 10);

        assert!(!dead.try_claim(9), "stale claim fails on the bumped word");
        assert!(dead.try_kill(9).is_none(), "stale kill too");

        dead.occupy(10);
        let fresh = dead.load();
        assert_eq!(fresh.state, Some(ProcessState::Created));
        assert!(dead.try_claim(10), "the new occupant claims normally");
        assert!(!dead.try_claim(9), "the old PID still cannot touch it");
    }

    #[test]
    fn is_alive_matches_generation_and_liveness() {
        assert!(word(4, ProcessState::Blocked, false).load().is_alive(4));
        assert!(!word(4, ProcessState::Blocked, false).load().is_alive(3));
        assert!(!word(4, ProcessState::Dead, false).load().is_alive(4));
        assert!(!LifecycleWord::new().load().is_alive(0));
    }
}
