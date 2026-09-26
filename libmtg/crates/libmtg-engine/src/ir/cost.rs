//! Cost-IR schema types.
//!
//! A `CostSchema` is the flat list of decisions the player must answer when
//! paying an `Action`-shaped cost. The schema is built by walking the cost
//! tree (`cost_exec::build_schema`) and answered by the strategy returning a
//! `BindEnv`. Once both exist, `cost_exec::pay` runs the cost via the regular
//! `executor::execute` — there is no second interpreter.
//!
//! Mana is intentionally NOT a `DecisionKind`. `Action::PayMana(mc)` reads
//! from the player's pool at execution time; if the pool can't pay, the
//! executor returns `ExecResult::ManaShortage` and the cost driver yields to
//! the strategy to activate mana abilities (each itself a PlayableAction).
//! The CR 601.2g sub-loop emerges from that interleaving.

use crate::ObjId;

/// Decisions the player must answer at announcement to pay a cost.
///
/// `decisions` is flat — order is announcement order. `Choose` nodes
/// expand into a single `Branch` decision plus the decisions inside the
/// chosen branch (collected after the chooser commits).
#[derive(Clone, Default)]
pub struct CostSchema {
    pub decisions: Vec<Decision>,
}

#[derive(Clone)]
pub struct Decision {
    /// `BindEnv` key the executor will read this decision's answer from.
    pub binding: &'static str,
    pub kind: DecisionKind,
}

#[derive(Clone)]
pub enum DecisionKind {
    /// Pick `count` distinct ObjIds from `candidates`. Used by Tap, Sacrifice,
    /// Discard, Exile, ReturnFromBattlefield — anything that targets one or
    /// more objects matching a Filter at announcement time.
    Objects {
        candidates: Vec<ObjId>,
        count: u32,
    },
    /// Pick one of several `labels` (typically the labels of `ChoiceOption`s).
    /// `payable` is the subset of indices whose branch is actually payable —
    /// the strategy MUST pick from that subset. `branches[i]` is the
    /// sub-schema of decisions *inside* option `i`'s action (e.g. the
    /// "which card to discard" pick of a discard branch); once the chooser
    /// commits to index `i`, only `branches[i]`'s decisions must be answered.
    /// Unpayable branches carry an empty sub-schema.
    Branch {
        labels: Vec<&'static str>,
        payable: Vec<usize>,
        branches: Vec<CostSchema>,
    },
    /// Pick a non-negative integer in `0..=max`. Used by X-costs and
    /// replicate-count.
    Number {
        kind: NumberKind,
        max: u32,
    },
}

/// What a `Number` decision represents — used by the strategy to pick
/// sensibly (e.g. a life-cost X is bounded by remaining life, mana-X by
/// available mana).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NumberKind {
    /// Pay `n` life. Bounded by `state.player(who).life - 1`.
    XLife,
    /// Pay `{n}` generic mana. Bounded by available mana.
    XMana,
    /// Replicate count (CR 702.58). Each unit also pays a copy of the cost.
    Replicate,
}

impl CostSchema {
    pub(crate) fn empty() -> Self {
        Self { decisions: Vec::new() }
    }

    pub(crate) fn push(&mut self, d: Decision) {
        self.decisions.push(d);
    }
}

/// Unrecoverable error from the cost executor. Distinct from `ManaShortage`
/// (which is recoverable by activating mana abilities).
#[derive(Clone, Debug)]
pub enum PayError {
    /// A `Decision` in the schema was not answered in `BindEnv`.
    MissingBinding(&'static str),
    /// A binding was present but had the wrong shape for its decision.
    WrongBindingShape(&'static str),
    /// A binding referenced an ObjId not in the decision's candidate set.
    BindingNotInCandidates {
        binding: &'static str,
        provided: ObjId,
    },
    /// A `Number` binding exceeded the decision's `max`.
    NumberOutOfRange {
        binding: &'static str,
        provided: u32,
        max: u32,
    },
    /// `Action::PayMana` reached but pool was short. Recovered upstream by
    /// the strategy activating mana abilities; if surfaced as a `PayError`,
    /// the strategy gave up.
    ManaShortage(crate::ManaCost),
}
