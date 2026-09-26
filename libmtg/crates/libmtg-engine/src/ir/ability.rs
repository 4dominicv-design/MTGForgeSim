//! Ability wrappers — the five CR ability kinds as a closed enum.

use crate::ir::action::Action;
use crate::ir::ce::CEMod;
use crate::ir::expr::{Expr, Filter};

/// A card-authored ability. One card has `Vec<Ability>`.
#[derive(Clone)]
pub struct Ability {
    pub kind: AbilityKind,
    pub text: Option<&'static str>, // Oracle snippet for docs / round-trip
}

/// The five CR ability kinds. Mirrors the event-timing taxonomy.
#[derive(Clone)]
pub enum AbilityKind {
    /// "When/whenever/at [event], [effect]." Fires *after* the event.
    Triggered {
        spec: TriggerSpec,
        /// Target spec for the triggered ability. Default `TargetSpec::None`
        /// for triggers whose body references the triggering event directly
        /// (no player choice).
        target_spec: crate::TargetSpec,
        body: Action,
        /// Zone in which the source must reside for this trigger to be armed.
        /// Battlefield for permanents (default); Stack for self-triggers on
        /// spells (storm, cascade, "when you cast this spell").
        active_zone: crate::ir::expr::ZoneKindSel,
    },
    /// "As [x], …" / "If [event] would happen, instead [other]." Modifies the
    /// event as it happens. Body is `ReplacementBody`, not `Action` — a
    /// replacement is a structured transformation, not an arbitrary effect.
    ///
    /// `condition` is an optional extra predicate evaluated against the match
    /// bindings — used for "unless X" wording where the replacement only fires
    /// when some game-state condition holds (e.g. Mistrise Village: enters
    /// tapped *unless* you control a Mountain or Forest).
    ///
    /// `active_zone` gates the *source* by zone (mirrors `Prohibition`):
    /// `Some(Battlefield)` for a static permanent whose replacement affects *other*
    /// objects (Leyline of the Void, Containment Priest), so it only functions in
    /// play. `None` for a self-entry replacement (Mistrise's "enters tapped"), where
    /// the source isn't yet on the battlefield when its own entry is replaced and the
    /// self-scoping `obj_filter` already pins relevance.
    Replacement {
        matches: EventPattern,
        condition: Option<Expr>,
        body: ReplacementBody,
        active_zone: Option<crate::ir::expr::ZoneKindSel>,
    },
    /// "[x] can't [y]." Prevents matching events from occurring at all.
    /// Consulted in `fire_event` Stage 1; a match suppresses the event (CR 614.17
    /// "can't" beats replacements / 101.2 "can't beats can").
    ///
    /// `active_zone` gates the *source* by zone, for prohibitions whose relevance
    /// depends on where their source sits: `Some(Battlefield)` for a static permanent
    /// prohibition (Grafdigger's Cage stops functioning once it leaves play). `None`
    /// when the pattern is self-gating — a self-scoping `It == Source` (Emrakul's "this
    /// spell can't be countered") already pins relevance to the one spell on the stack
    /// being countered, so no zone check is needed (and a Stack check would be wrong
    /// for callers that fake a stack spell without a `StackSpell` role).
    Prohibition {
        matches: EventPattern,
        active_zone: Option<crate::ir::expr::ZoneKindSel>,
    },
    /// Action-restriction (CR's "restriction" — 508.1d/509.1c pair restrictions with
    /// requirements; CR 601/602.5): the controller of a `subject` object can't take
    /// player action `action` with it (can't cast / can't activate / …). Consulted
    /// where legal options are *produced* (`is_legal` / enumeration), as an AND-NOT
    /// gate over *permission*, so "can't beats can" (CR 101.2) is order-independent —
    /// distinct from `Prohibition`, which suppresses a fired *event* in the pipeline.
    /// `subject` is evaluated with the candidate object as `Ctx::It` and the
    /// restriction's source/controller bound (so "opponent's" = `Controller(It) ≠
    /// Ctx::Controller`). For `Activate`, the bool var `activating_mana_ability` is
    /// also bound (true while the mana sub-loop is the caller — CR 605.1a), so the
    /// "… unless they're mana abilities" rider (Pithing Needle, Disruptor Flute) is
    /// just a subject clause `Not(Ctx::Var("activating_mana_ability"))` — no flag.
    Restriction {
        action: ActionKind,
        subject: Filter,
    },
    /// Continuous effect: while source is active, apply these CE mods.
    Static {
        mods: Vec<CEMod>,
        /// Scope: what the CE applies to. `None` = global; else filter on
        /// candidate objects/players.
        scope: Option<Filter>,
        /// Global activation gate (CR 613 "as long as …"), re-evaluated each
        /// recompute against the source's binding frame. `None` = always active;
        /// `Some(e)` = the whole block contributes nothing while `e` is false
        /// (e.g. delirium, metalcraft, "during your turn"). Distinct from `scope`,
        /// which decides *which objects* a (then-active) effect touches.
        condition: Option<Expr>,
    },
    /// "[cost]: [effect]." Mana abilities (CR 605.1a) are NOT a separate
    /// variant — they are activated abilities whose body could produce mana
    /// and whose `target_spec` is `TargetSpec::None`. The executor classifies
    /// them via `is_mana_ability` and routes through the synchronous
    /// stack-bypass path (CR 605.3b) automatically.
    Activated {
        cost: CostBody,
        /// Target spec for the ability. `TargetSpec::None` is the no-target
        /// case (Clue Token, Karn's +1, Birds of Paradise).
        target_spec: crate::TargetSpec,
        /// Resolution-time object choice ("Choose an exiled card …" — CR
        /// 701.10). Distinct from `target_spec`, which is announcement-time
        /// targeting (CR 113 / 601.2c). Chosen id is passed to the body via
        /// the `target` binding alongside announced targets.
        choice_spec: Option<crate::ChoiceSpec>,
        body: Action,
        /// Activation timing. `Default` = instant speed and allowed in the
        /// mana sub-loop (CR 601.2g) when this is a mana ability. `Instant` =
        /// instant speed but excluded from the mana sub-loop (Lion's Eye
        /// Diamond). `Sorcery` = main phase, empty stack only.
        timing: crate::ActivationTiming,
        /// "Activate only if X" restriction (Mox Opal metalcraft). `None` =
        /// always activatable (subject to other costs/timing).
        activation_condition: Option<Expr>,
        /// Zone the source must reside in to activate. Default: Battlefield.
        /// Hand for Simian Spirit Guide-style cards.
        active_zone: crate::ir::expr::ZoneKindSel,
    },
    /// Spell resolution body. Not a CR-112 ability — a spell's effect on
    /// resolution belongs to the spell itself, not to an ability it has —
    /// but represented in `AbilityKind` for uniform engine dispatch.
    ///
    /// `modes` mirrors `catalog::SpellModes`: non-modal spells have
    /// `modes.len() == 1`; modal spells (CR 700.2) have one entry per mode.
    /// The cast submachine selects the mode index; target_spec for that mode
    /// governs target selection.
    OnResolve {
        modes: Vec<IrSpellMode>,
    },
}

/// A player action an `AbilityKind::Restriction` can forbid (CR 601 cast / 602.5
/// activate / 508–509 attack-block). Grows as cards need it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActionKind {
    Cast,
    Activate,
    Attack,
}

/// Trigger specification — what event fires this ability.
#[derive(Clone)]
pub enum TriggerSpec {
    /// Event pattern + optional extra condition (typically controller scope
    /// or object identity).
    When {
        pattern: EventPattern,
        condition: Option<Expr>,
    },
    /// Phase / step triggers ("at the beginning of your upkeep").
    AtStep {
        step: crate::StepKind,
        who: StepScope,
        /// Optional intervening-if (CR 603.4) evaluated against the source's
        /// binding frame — the trigger doesn't fire when it's false (Delver's
        /// "if an instant or sorcery is on top"). `None` = unconditional.
        condition: Option<Expr>,
    },
}

/// Which player's step triggers this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepScope {
    You,
    EachOpponent,
    EachPlayer,
    ActivePlayer,
}

/// Pattern over `GameEvent`. Matches structurally — the engine translates each
/// `GameEvent` variant into field bindings for the enclosing expression tree.
#[derive(Clone)]
pub enum EventPattern {
    /// Any event — used rarely (Orcish Bowmasters fires on any draw).
    Any,

    /// An object enters a zone.
    EntersZone {
        obj_filter: Filter,
        zone_kind: crate::ir::expr::ZoneKindSel,
    },
    /// An object leaves a zone.
    LeavesZone {
        obj_filter: Filter,
        zone_kind: crate::ir::expr::ZoneKindSel,
    },
    /// An object moves between specific zones (CR 603.6a). More precise than
    /// EntersZone/LeavesZone alone when the trigger cares about the *pair*.
    /// `actor_filter` (optional) matches the player who caused the move; used
    /// for "whenever you exile X" style triggers.
    ZoneChange {
        obj_filter: Filter,
        from: crate::ir::expr::ZoneKindSel,
        to: crate::ir::expr::ZoneKindSel,
        actor_filter: Option<Filter>,
    },
    /// A creature dies (leaves battlefield for graveyard).
    Dies {
        obj_filter: Filter,
    },
    /// A spell is cast.
    SpellCast {
        spell_filter: Filter,
    },
    /// A spell on the stack is about to be countered (CR 701.5). Fired by
    /// `counter_one` before removal; a matching `AbilityKind::Prohibition`
    /// suppresses it ("this spell can't be countered" — Emrakul, Long Goodbye).
    /// `spell_filter` runs with the countered spell as the subject (`Ctx::It`),
    /// so `It == Source` self-scopes the prohibition to its own spell.
    SpellBeingCountered {
        spell_filter: Filter,
    },
    /// A player draws one or more cards.
    Draw {
        who: Filter, // predicate on player
    },
    /// A player plays a land.
    LandPlayed {
        who: Filter,
        land_filter: Filter,
    },
    /// Damage dealt.
    DamageDealt {
        source_filter: Filter,
        target_filter: Filter,
        is_combat: Option<bool>,
    },
    /// Creature attacks.
    Attacks {
        attacker_filter: Filter,
    },
    /// Creature blocks.
    Blocks {
        blocker_filter: Filter,
    },

    /// Conjunction — all of these patterns must match simultaneously.
    And(Vec<EventPattern>),

    /// Disjunction — matches if any sub-pattern matches; the first match's
    /// bindings are returned. Used where one rules ability ranges over
    /// alternative events, e.g. Grafdigger's "from graveyards *or* libraries"
    /// (one `ZoneChange` per source zone) — keeps it a single CR ability rather
    /// than splitting into two.
    Or(Vec<EventPattern>),
}

/// How a replacement effect changes the event.
#[derive(Clone)]
pub enum ReplacementBody {
    /// Replace the event with a different action. The action sees the matched
    /// event's bindings (e.g. `Var("triggered_obj")`) via the `BindEnv`
    /// populated by `match_event_pattern`. Used for: "enters tapped" composed
    /// as `Sequence([Move(target → BF), Tap(target)])`; Leyline of the Void
    /// composed as `Sequence([Move(target → Exile), …])`; Containment Priest
    /// "exile instead of entering."
    ///
    /// CR 614.5 self-loop guard is engine-enforced — the same replacement
    /// won't re-fire on events the action body produces.
    Replace(Action),
    /// Prevent (CR 615) — damage/effect does not occur.
    Prevent,
}

/// Cost of an activated ability or alternate spell cost. Single-variant
/// `Ir(Action)` enum after Phase 6 collapsed the dual world. Kept as an
/// enum (rather than a transparent newtype) so existing match arms
/// `let CostBody::Ir(action) = &x.costs` continue to read clearly; future
/// cleanup can make this a struct or alias.
#[derive(Clone)]
pub enum CostBody {
    Ir(Action),
}

impl Default for CostBody {
    fn default() -> Self {
        CostBody::Ir(Action::Noop)
    }
}

impl CostBody {
    /// Empty cost — no payment, no decisions. Used by `Default for
    /// AlternateCost` (Omniscience's free-cast grant), default `AbilityDef`,
    /// default `ManaAbility`, etc.
    pub(crate) fn empty() -> Self {
        CostBody::Ir(Action::Noop)
    }

    /// True if this cost is structurally empty (no payment, no decisions).
    pub(crate) fn is_empty(&self) -> bool {
        matches!(self, CostBody::Ir(Action::Noop))
    }

    /// True iff this cost requires tapping the source — used by the mana
    /// affordability predictor (`accumulate_source_potential`) to skip
    /// already-tapped sources.
    pub fn requires_tap_self(&self) -> bool {
        let CostBody::Ir(a) = self;
        action_includes_tap_source(a)
    }

    /// True iff this cost requires sacrificing the source — used by the
    /// affordability predictor to mark a source as no longer available
    /// after activation.
    pub fn requires_sac_self(&self) -> bool {
        let CostBody::Ir(a) = self;
        action_includes_sac_source(a)
    }

    /// True iff payment of this cost involves any mana spend. Used by
    /// `cast_spell` to set `SpellCast::mana_spent` correctly.
    pub(crate) fn includes_mana(&self) -> bool {
        let CostBody::Ir(a) = self;
        action_includes_pay_mana(a)
    }

    /// Extract the (first) mana cost component, if any. Used by `cast_spell`
    /// when computing the `mana_cost` to drain from the pool for the
    /// alt-cost path.
    pub(crate) fn first_mana_cost(&self) -> Option<crate::ManaCost> {
        let CostBody::Ir(a) = self;
        first_pay_mana(a)
    }

    /// True iff this cost contains a variable-X payment — a `PayLife` or
    /// `PayManaX` whose amount is not a compile-time constant. Used by the
    /// announce step to decide whether to ask the strategy for an X value
    /// (CR 601.2b).
    pub(crate) fn has_x_cost(&self) -> bool {
        let CostBody::Ir(a) = self;
        action_has_x_cost(a)
    }

    /// True iff this cost contains a variable-X generic mana payment.
    /// Kept separate from `has_x_cost()` so the casting layer can combine
    /// base mana and X mana before deciding affordability.
    pub(crate) fn has_x_mana_cost(&self) -> bool {
        let CostBody::Ir(a) = self;
        action_has_x_mana_cost(a)
    }

    /// True iff this cost contains a variable-X life payment.
    pub(crate) fn has_x_life_cost(&self) -> bool {
        let CostBody::Ir(a) = self;
        action_has_x_life_cost(a)
    }

    /// The replicate cost (CR 702.58), if this cost contains a `Replicate`.
    /// `cast_spell` uses it to push spell copies for each extra payment.
    pub(crate) fn replicate_mana_cost(&self) -> Option<crate::ManaCost> {
        let CostBody::Ir(a) = self;
        action_replicate_cost(a)
    }
}

fn action_includes_sac_source(a: &Action) -> bool {
    use crate::ir::action::Action::*;
    use crate::ir::action::MoveVerb;
    use crate::ir::expr::ZoneKindSel;
    let filter_self = |f: &crate::ir::expr::Filter| {
        let crate::ir::expr::Filter(expr) = f;
        let crate::ir::expr::Expr::Eq(lhs, rhs) = expr else { return false };
        let l_is_it = matches!(lhs.as_ref(), crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::It));
        let r_is_src = matches!(rhs.as_ref(), crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::Source));
        let l_is_src = matches!(lhs.as_ref(), crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::Source));
        let r_is_it = matches!(rhs.as_ref(), crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::It));
        (l_is_it && r_is_src) || (l_is_src && r_is_it)
    };
    match a {
        Sacrifice { filter, .. } if filter_self(filter) => true,
        MoveByChoice { from: ZoneKindSel::Battlefield, to: ZoneKindSel::Graveyard,
                       verb: MoveVerb::Sacrifice, filter, .. } if filter_self(filter) => true,
        Sequence(actions) => actions.iter().any(action_includes_sac_source),
        IfThen { then, else_, .. } => {
            action_includes_sac_source(then)
                || else_.as_ref().map_or(false, |e| action_includes_sac_source(e))
        }
        MayDo { action, .. } => action_includes_sac_source(action),
        ForEach { body, .. } => action_includes_sac_source(body),
        Choose { options, .. } => options.iter().any(|o| action_includes_sac_source(&o.action)),
        _ => false,
    }
}

fn action_includes_tap_source(a: &Action) -> bool {
    use crate::ir::action::Action::*;
    match a {
        Tap { target } => matches!(
            target,
            crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::Source)
        ),
        Sequence(actions) => actions.iter().any(action_includes_tap_source),
        IfThen { then, else_, .. } => {
            action_includes_tap_source(then)
                || else_.as_ref().map_or(false, |e| action_includes_tap_source(e))
        }
        MayDo { action, .. } => action_includes_tap_source(action),
        ForEach { body, .. } => action_includes_tap_source(body),
        Choose { options, .. } => options
            .iter()
            .any(|o| action_includes_tap_source(&o.action)),
        _ => false,
    }
}

fn action_includes_pay_mana(a: &Action) -> bool {
    use crate::ir::action::Action::*;
    match a {
        PayMana(_) => true,
        Sequence(actions) => actions.iter().any(action_includes_pay_mana),
        IfThen { then, else_, .. } => {
            action_includes_pay_mana(then)
                || else_.as_ref().map_or(false, |e| action_includes_pay_mana(e))
        }
        MayDo { action, .. } => action_includes_pay_mana(action),
        ForEach { body, .. } => action_includes_pay_mana(body),
        Choose { options, .. } => options
            .iter()
            .any(|o| action_includes_pay_mana(&o.action)),
        _ => false,
    }
}

/// Walk the cost tree for the first `PayMana(mc)` and return `mc`. Used by
/// `cast_spell` and effects.rs to know the mana cost for affordability /
/// alt-cost extraction. Exposed for free-function callers; mirrors
/// `CostBody::first_mana_cost`.
pub(crate) fn first_pay_mana_in_action(a: &Action) -> Option<crate::ManaCost> {
    first_pay_mana(a)
}

fn first_pay_mana(a: &Action) -> Option<crate::ManaCost> {
    use crate::ir::action::Action::*;
    match a {
        PayMana(mc) => Some(mc.clone()),
        Sequence(actions) => actions.iter().find_map(first_pay_mana),
        IfThen { then, else_, .. } => first_pay_mana(then)
            .or_else(|| else_.as_ref().and_then(|e| first_pay_mana(e))),
        MayDo { action, .. } => first_pay_mana(action),
        ForEach { body, .. } => first_pay_mana(body),
        Choose { options, .. } => options.iter().find_map(|o| first_pay_mana(&o.action)),
        _ => None,
    }
}

fn action_has_x_cost(a: &Action) -> bool {
    use crate::ir::action::Action::*;
    use crate::ir::expr::Expr;
    // X = a payment amount that is not a literal — the variable announced at
    // CR 601.2b. Constants (`Expr::Num`) are fixed costs, not X.
    let is_variable = |e: &Expr| !matches!(e, Expr::Num(_));
    match a {
        PayLife { amount, .. } => is_variable(amount),
        PayManaX { generic } => is_variable(generic),
        Sequence(actions) => actions.iter().any(action_has_x_cost),
        IfThen { then, else_, .. } => {
            action_has_x_cost(then) || else_.as_ref().map_or(false, |e| action_has_x_cost(e))
        }
        MayDo { action, .. } => action_has_x_cost(action),
        ForEach { body, .. } => action_has_x_cost(body),
        Choose { options, .. } => options.iter().any(|o| action_has_x_cost(&o.action)),
        _ => false,
    }
}

fn action_has_x_mana_cost(a: &Action) -> bool {
    use crate::ir::action::Action::*;
    use crate::ir::expr::Expr;
    let is_variable = |e: &Expr| !matches!(e, Expr::Num(_));
    match a {
        PayManaX { generic } => is_variable(generic),
        Sequence(actions) => actions.iter().any(action_has_x_mana_cost),
        IfThen { then, else_, .. } => {
            action_has_x_mana_cost(then)
                || else_.as_ref().map_or(false, |e| action_has_x_mana_cost(e))
        }
        MayDo { action, .. } => action_has_x_mana_cost(action),
        ForEach { body, .. } => action_has_x_mana_cost(body),
        Choose { options, .. } => options.iter().any(|o| action_has_x_mana_cost(&o.action)),
        _ => false,
    }
}

fn action_has_x_life_cost(a: &Action) -> bool {
    use crate::ir::action::Action::*;
    use crate::ir::expr::Expr;
    let is_variable = |e: &Expr| !matches!(e, Expr::Num(_));
    match a {
        PayLife { amount, .. } => is_variable(amount),
        Sequence(actions) => actions.iter().any(action_has_x_life_cost),
        IfThen { then, else_, .. } => {
            action_has_x_life_cost(then)
                || else_.as_ref().map_or(false, |e| action_has_x_life_cost(e))
        }
        MayDo { action, .. } => action_has_x_life_cost(action),
        ForEach { body, .. } => action_has_x_life_cost(body),
        Choose { options, .. } => options.iter().any(|o| action_has_x_life_cost(&o.action)),
        _ => false,
    }
}

fn action_replicate_cost(a: &Action) -> Option<crate::ManaCost> {
    use crate::ir::action::Action::*;
    match a {
        Replicate(mc) => Some(mc.clone()),
        Sequence(actions) => actions.iter().find_map(action_replicate_cost),
        IfThen { then, else_, .. } => action_replicate_cost(then)
            .or_else(|| else_.as_ref().and_then(|e| action_replicate_cost(e))),
        MayDo { action, .. } => action_replicate_cost(action),
        ForEach { body, .. } => action_replicate_cost(body),
        Choose { options, .. } => options.iter().find_map(|o| action_replicate_cost(&o.action)),
        _ => None,
    }
}

/// One mode of a spell under `AbilityKind::OnResolve`.
#[derive(Clone)]
pub struct IrSpellMode {
    pub target_spec: crate::TargetSpec,
    pub body: Action,
}
