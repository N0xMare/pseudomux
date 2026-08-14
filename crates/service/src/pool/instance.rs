//! One pool instance: its identity, its paths, and the invariants it carries.
//!
//! # Why epochs
//!
//! `require_pristine_root_for_minified_cell` refuses a config root containing
//! anything but the two seeded files. A partially-failed delete under
//! `<slot>/` would therefore refuse every future mint into that slot forever.
//! With `<slot>/<epoch>/`, a failed delete costs disk and an operator alert; it
//! never costs the slot. The epoch also guarantees a surviving orphan can never
//! share a directory with a new instance.

use std::path::{Path, PathBuf};

use super::class::InstanceClass;
use super::host::InstanceHandle;
use super::machine::{INITIAL, InstanceState, Transition};

/// `0..pool_size`. pmux-internal; never on the wire.
pub type SlotId = u32;
/// Increments on every mint into a slot. pmux-internal; never on the wire.
pub type Epoch = u64;

/// The three paths one instance owns, all derived from operator config plus a
/// slot identity with no request byte in them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotPaths {
    /// `<pool_parent>/<slot>/<epoch>`
    pub epoch_dir: PathBuf,
    /// `<pool_parent>/<slot>/<epoch>/root` -- `CLAUDE_CONFIG_DIR`.
    pub root: PathBuf,
    /// `<pool_parent>/<slot>/<epoch>/cwd`
    pub cwd: PathBuf,
    /// `<pool_parent>/<slot>/<epoch>/pid`
    pub pid_file: PathBuf,
}

impl SlotPaths {
    /// Derive the paths for one `(slot, epoch)` under a pool parent.
    ///
    /// pmux creating these directories is not a violation of `ConfigIsolation`'s
    /// "pmux never creates the root" rule, and the difference is precise: that
    /// rule exists so `start_session` does not gain a filesystem-write
    /// capability over a **caller-named** tree on the admission path. This path
    /// is operator config plus slot plus epoch, and no caller can name it.
    #[must_use]
    pub fn new(parent: &Path, slot: SlotId, epoch: Epoch) -> Self {
        let epoch_dir = parent.join(slot.to_string()).join(epoch.to_string());
        Self {
            root: epoch_dir.join("root"),
            cwd: epoch_dir.join("cwd"),
            pid_file: epoch_dir.join("pid"),
            epoch_dir,
        }
    }

    /// Every directory pmux creates under `parent` for this instance, outermost
    /// first, or `None` when a path is not under `parent` at all.
    ///
    /// **DERIVED by walking each leaf's ancestors down from `parent`, and
    /// deliberately not a list.** A list is what shipped `0755`
    /// `<parent>/<slot>` directories: `mint_roots` sealed the three paths
    /// somebody wrote down (`epoch_dir`, `root`, `cwd`), `create_dir_all`
    /// silently created the fourth (`<parent>/<slot>`), and the test that
    /// "proved" the tree owner-only walked the same three-element array -- so
    /// the one level in the chain that was not sealed was the one absent from
    /// the list. An ancestor walk cannot omit an intermediate level, because
    /// intermediate levels are exactly what it enumerates.
    ///
    /// `mint_roots` creates and seals exactly this, and the test that proves
    /// the tree owner-only walks exactly this. One derivation, both callers.
    ///
    /// `None` rather than a partial answer when a leaf escapes `parent`: the
    /// return value is fed to `mkdir` and `chmod`, and a chain that walked past
    /// `parent` would walk to `/`. It is the same containment question
    /// `erase_tree` asks before `remove_dir_all`, asked before creation instead
    /// of before deletion.
    #[must_use]
    pub fn minted_dirs(&self, parent: &Path) -> Option<Vec<PathBuf>> {
        // Exhaustive destructure, not field access: a directory added to this
        // struct is a compile error here until somebody classifies it, which is
        // the only mechanism that makes "every directory pmux creates" a
        // checkable claim rather than a comment.
        let Self {
            epoch_dir: _,
            root,
            cwd,
            // A FILE, written by `mint` after the child exists. Its directory is
            // `epoch_dir`, which the two leaves below already reach.
            pid_file: _,
        } = self;

        let mut chain: Vec<PathBuf> = Vec::new();
        for leaf in [root, cwd] {
            let mut upwards: Vec<&Path> = Vec::new();
            let mut reached_parent = false;
            for ancestor in leaf.ancestors() {
                if ancestor == parent {
                    reached_parent = true;
                    break;
                }
                upwards.push(ancestor);
            }
            if !reached_parent {
                return None;
            }
            for directory in upwards.into_iter().rev() {
                if !chain.iter().any(|held| held == directory) {
                    chain.push(directory.to_path_buf());
                }
            }
        }
        Some(chain)
    }
}

/// One instance, as the pool models it.
#[derive(Clone, Debug)]
pub struct Instance {
    pub slot: SlotId,
    pub epoch: Epoch,
    /// Immutable for the process's whole life: `--model` and `--effort` are
    /// launch-time argv and `/clear` does not re-exec.
    pub class: InstanceClass,
    /// The fingerprint of the daemon system prompt this instance was minted
    /// under. Compared against live configuration before the instance may
    /// re-enter the idle set, so a configuration reload cannot leave an
    /// instance serving under a prompt the daemon no longer holds.
    pub prompt_fingerprint: u64,
    pub paths: SlotPaths,
    /// `None` until the mint returns. Its presence is what distinguishes a
    /// reservation from a process.
    pub handle: Option<InstanceHandle>,
    /// Whether an `InstanceHost::mint` call is outstanding right now.
    ///
    /// **`handle.is_none()` cannot answer "was a process ever launched".** It
    /// is equally false before the launch starts and for the whole width of it,
    /// and the pool holds no lock across a launch, so a concurrent teardown
    /// reaches an instance in the second case reading it as the first. This is
    /// the bit that tells them apart, and teardown is its only reader: a
    /// destroy that finds it set cannot prove the boundary empty, so it leaks
    /// the slot and keeps the tree rather than erasing a root a child may be
    /// writing into.
    ///
    /// Set only in `Warming`, and cleared by whichever of the two mint
    /// outcomes arrives: a handle supersedes it, and a failure carries
    /// `HostFailure::process_may_survive`, which is a better answer than "a
    /// launch is in flight" because the host measured it.
    pub mint_in_flight: bool,
    /// Incremented at CHECKOUT, not at check-in.
    ///
    /// The counter's job is to bound how many distinct callers' prompts coexist
    /// in this root's `history.jsonl`, and a prompt reaches that file at
    /// submission -- the writer is append-only under a lock and `/clear` is
    /// itself appended as a row. A counter incremented at check-in miscounts a
    /// turn that was submitted and then failed.
    pub turns_started: u32,
    /// Set on every entry into the idle set. LRU victim choice and the TTL
    /// sweep read it; nothing else does.
    pub idle_since_ms: u64,
    pub state: InstanceState,
    /// The transition that produced `state`. Read by the idle-set invariant, so
    /// "this instance carries a proof" is a fact about how it got here rather
    /// than a flag somebody set.
    pub last_transition: Option<Transition>,
    /// Whether this instance ever passed through `Quarantined`.
    ///
    /// Sticky, and read at teardown to decide retention. It cannot be derived
    /// from `state` or `last_transition`, because quarantine's only exit is
    /// `BeginDestroy` -- by the time the tree is erased, both fields describe
    /// the teardown rather than the reason for it, and the difference between
    /// "erase with no floor" and "keep as evidence" would be lost.
    pub was_quarantined: bool,
}

/// A per-instance invariant that did not hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvariantViolation {
    /// An instance sits in the idle set without a proof-carrying transition.
    IdleWithoutProof {
        slot: SlotId,
        last_transition: Option<Transition>,
    },
    /// An instance is idle at or past its recycle cap, so a checkout would hand
    /// out an instance that should have been torn down.
    IdleAtOrPastRecycleCap {
        slot: SlotId,
        turns_started: u32,
        recycle_turns: u32,
    },
    /// An instance is idle under a system prompt the daemon no longer holds.
    IdleUnderStalePrompt { slot: SlotId },
    /// A state that requires a process has no handle.
    MissingHandle { slot: SlotId, state: InstanceState },
    /// A reservation acquired a handle without passing through a mint.
    ReservationWithHandle { slot: SlotId },
    /// A turn is in flight on an instance that never counted a checkout.
    InFlightWithoutCheckout { slot: SlotId, state: InstanceState },
}

impl std::fmt::Display for InvariantViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdleWithoutProof {
                slot,
                last_transition,
            } => write!(
                formatter,
                "slot {slot} is idle after {last_transition:?}, which carries no emptiness proof"
            ),
            Self::IdleAtOrPastRecycleCap {
                slot,
                turns_started,
                recycle_turns,
            } => write!(
                formatter,
                "slot {slot} is idle at {turns_started} of {recycle_turns} turns and should have been recycled"
            ),
            Self::IdleUnderStalePrompt { slot } => write!(
                formatter,
                "slot {slot} is idle under a system prompt the daemon no longer holds"
            ),
            Self::MissingHandle { slot, state } => {
                write!(formatter, "slot {slot} is {state} with no process handle")
            }
            Self::ReservationWithHandle { slot } => write!(
                formatter,
                "slot {slot} holds a process handle while still only reserved"
            ),
            Self::InFlightWithoutCheckout { slot, state } => write!(
                formatter,
                "slot {slot} is {state} without having counted a checkout"
            ),
        }
    }
}

impl std::error::Error for InvariantViolation {}

impl Instance {
    /// A fresh reservation: a slot and an epoch, and nothing else.
    #[must_use]
    pub fn reserved(
        slot: SlotId,
        epoch: Epoch,
        class: InstanceClass,
        paths: SlotPaths,
        prompt_fingerprint: u64,
    ) -> Self {
        Self {
            slot,
            epoch,
            class,
            prompt_fingerprint,
            paths,
            handle: None,
            mint_in_flight: false,
            turns_started: 0,
            idle_since_ms: 0,
            state: INITIAL,
            last_transition: None,
            was_quarantined: false,
        }
    }

    /// The per-state invariants, checked after every transition.
    ///
    /// Every arm is stated positively: a state is admitted because something is
    /// true of it, not because nothing obviously wrong was noticed.
    ///
    /// # Errors
    ///
    /// Returns the first violated invariant.
    pub fn check_invariants(
        &self,
        recycle_turns: u32,
        live_prompt_fingerprint: u64,
    ) -> Result<(), InvariantViolation> {
        match self.state {
            InstanceState::Reserved => {
                if self.handle.is_some() {
                    return Err(InvariantViolation::ReservationWithHandle { slot: self.slot });
                }
            }
            InstanceState::Warming
            | InstanceState::Quarantined
            | InstanceState::Destroying
            | InstanceState::Leaked
            | InstanceState::Retired => {}
            InstanceState::Idle => {
                // The invariant the whole design rests on: membership in the
                // idle set IS the emptiness proof, so an idle instance must
                // have arrived through a proof-carrying transition.
                if !self
                    .last_transition
                    .is_some_and(Transition::is_proof_carrying)
                {
                    return Err(InvariantViolation::IdleWithoutProof {
                        slot: self.slot,
                        last_transition: self.last_transition,
                    });
                }
                if self.handle.is_none() {
                    return Err(InvariantViolation::MissingHandle {
                        slot: self.slot,
                        state: self.state,
                    });
                }
                if self.turns_started >= recycle_turns {
                    return Err(InvariantViolation::IdleAtOrPastRecycleCap {
                        slot: self.slot,
                        turns_started: self.turns_started,
                        recycle_turns,
                    });
                }
                if self.prompt_fingerprint != live_prompt_fingerprint {
                    return Err(InvariantViolation::IdleUnderStalePrompt { slot: self.slot });
                }
            }
            InstanceState::CheckedOut | InstanceState::Delivering | InstanceState::Clearing => {
                if self.handle.is_none() {
                    return Err(InvariantViolation::MissingHandle {
                        slot: self.slot,
                        state: self.state,
                    });
                }
                if self.turns_started == 0 {
                    return Err(InvariantViolation::InFlightWithoutCheckout {
                        slot: self.slot,
                        state: self.state,
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::class::resolve_pool_class;
    use pseudomux_protocol::v1::{EffortLevel, SessionGenerationId, SessionId};

    fn class() -> InstanceClass {
        resolve_pool_class("claude-opus-5", Some(EffortLevel::High))
            .expect("admitted")
            .0
    }

    fn handle() -> InstanceHandle {
        InstanceHandle {
            session_id: SessionId::new_v4(),
            generation_id: SessionGenerationId::default(),
            pid: Some(4_242),
            claude_version: "2.1.220".to_owned(),
        }
    }

    fn instance() -> Instance {
        Instance::reserved(
            3,
            7,
            class(),
            SlotPaths::new(Path::new("/pool"), 3, 7),
            0xfeed,
        )
    }

    #[test]
    fn a_slot_and_epoch_name_exactly_one_tree() {
        let paths = SlotPaths::new(Path::new("/pool"), 3, 7);
        assert_eq!(paths.epoch_dir, PathBuf::from("/pool/3/7"));
        assert_eq!(paths.root, PathBuf::from("/pool/3/7/root"));
        assert_eq!(paths.cwd, PathBuf::from("/pool/3/7/cwd"));
        assert_eq!(paths.pid_file, PathBuf::from("/pool/3/7/pid"));

        // The property epochs exist for: a new mint into the same slot never
        // shares a directory with the old one, so a failed delete costs disk
        // and never the slot.
        let next = SlotPaths::new(Path::new("/pool"), 3, 8);
        assert_ne!(next.epoch_dir, paths.epoch_dir);
        assert!(!next.root.starts_with(&paths.epoch_dir));
    }

    #[test]
    fn a_reservation_owns_no_process() {
        let instance = instance();
        assert_eq!(instance.state, InstanceState::Reserved);
        assert!(instance.handle.is_none());
        assert_eq!(instance.turns_started, 0);
        instance
            .check_invariants(50, 0xfeed)
            .expect("a fresh reservation is admissible");
    }

    #[test]
    fn a_reservation_holding_a_handle_is_refused() {
        let mut instance = instance();
        instance.handle = Some(handle());
        assert_eq!(
            instance.check_invariants(50, 0xfeed),
            Err(InvariantViolation::ReservationWithHandle { slot: 3 })
        );
    }

    #[test]
    fn an_idle_instance_must_have_arrived_through_a_proof() {
        let mut instance = instance();
        instance.state = InstanceState::Idle;
        instance.handle = Some(handle());

        // Every non-proof transition is refused...
        for transition in [
            Transition::CheckOut,
            Transition::TurnCommitted,
            Transition::ResponseDelivered,
            Transition::BeginWarm,
            Transition::RecycleDue,
            Transition::ClearFailedCoherent,
            Transition::BeginDestroy,
        ] {
            instance.last_transition = Some(transition);
            assert_eq!(
                instance.check_invariants(50, 0xfeed),
                Err(InvariantViolation::IdleWithoutProof {
                    slot: 3,
                    last_transition: Some(transition),
                }),
                "{transition} must not be admissible as an entry into the idle set"
            );
        }
        // ...and no transition at all is refused too, so "never transitioned"
        // cannot masquerade as a proof.
        instance.last_transition = None;
        assert_eq!(
            instance.check_invariants(50, 0xfeed),
            Err(InvariantViolation::IdleWithoutProof {
                slot: 3,
                last_transition: None,
            })
        );

        // Exactly the two proof-carrying transitions are admitted.
        for transition in Transition::PROOF_CARRYING {
            instance.last_transition = Some(transition);
            instance
                .check_invariants(50, 0xfeed)
                .expect("a proof-carrying transition admits the idle set");
        }
    }

    #[test]
    fn an_idle_instance_at_the_recycle_cap_is_refused() {
        let mut instance = instance();
        instance.state = InstanceState::Idle;
        instance.handle = Some(handle());
        instance.last_transition = Some(Transition::ClearProven);
        instance.turns_started = 50;
        assert_eq!(
            instance.check_invariants(50, 0xfeed),
            Err(InvariantViolation::IdleAtOrPastRecycleCap {
                slot: 3,
                turns_started: 50,
                recycle_turns: 50,
            })
        );
        instance.turns_started = 49;
        instance
            .check_invariants(50, 0xfeed)
            .expect("one turn below the cap is still serviceable");
    }

    #[test]
    fn an_idle_instance_under_a_stale_prompt_is_refused() {
        let mut instance = instance();
        instance.state = InstanceState::Idle;
        instance.handle = Some(handle());
        instance.last_transition = Some(Transition::WarmProven);
        assert_eq!(
            instance.check_invariants(50, 0xbeef),
            Err(InvariantViolation::IdleUnderStalePrompt { slot: 3 })
        );
    }

    #[test]
    fn a_state_that_needs_a_process_is_refused_without_one() {
        for state in [
            InstanceState::Idle,
            InstanceState::CheckedOut,
            InstanceState::Delivering,
            InstanceState::Clearing,
        ] {
            let mut instance = instance();
            instance.state = state;
            instance.last_transition = Some(Transition::WarmProven);
            instance.turns_started = 1;
            assert_eq!(
                instance.check_invariants(50, 0xfeed),
                Err(InvariantViolation::MissingHandle { slot: 3, state }),
                "{state} must carry a process handle"
            );
        }
    }

    #[test]
    fn a_turn_in_flight_must_have_counted_its_checkout() {
        for state in [
            InstanceState::CheckedOut,
            InstanceState::Delivering,
            InstanceState::Clearing,
        ] {
            let mut instance = instance();
            instance.state = state;
            instance.handle = Some(handle());
            instance.turns_started = 0;
            assert_eq!(
                instance.check_invariants(50, 0xfeed),
                Err(InvariantViolation::InFlightWithoutCheckout { slot: 3, state }),
                "{state} must follow a counted checkout"
            );
            instance.turns_started = 1;
            instance
                .check_invariants(50, 0xfeed)
                .expect("a counted checkout admits an in-flight turn");
        }
    }

    #[test]
    fn teardown_states_carry_no_positive_requirement() {
        // Quarantine, destruction and leakage are reached from failures, so
        // demanding a well-formed handle there would refuse exactly the
        // instances that most need tearing down.
        for state in [
            InstanceState::Quarantined,
            InstanceState::Destroying,
            InstanceState::Leaked,
            InstanceState::Retired,
        ] {
            let mut instance = instance();
            instance.state = state;
            instance.handle = None;
            instance
                .check_invariants(50, 0xfeed)
                .expect("a teardown state is always admissible");
        }
    }
}
