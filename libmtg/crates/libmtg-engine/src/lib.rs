//! A generic, reusable rules engine for Magic: The Gathering.
//!
//! `libmtg-engine` owns the game model and the rules machinery, with **no**
//! game-specific strategy, deck database, or UI: cards and zones, mana and the
//! stack, the replacement/trigger/continuous-effect infrastructure, and the
//! [`SimState`] / [`Strategy`] / [`Objective`] API for driving game state and
//! decisions. Card behavior is expressed as data in a small IR (see the [`ir`]
//! module) interpreted by a generic executor.
//!
//! Concrete content (a specific deck's strategy, objectives, and scenarios) lives
//! in consumer crates such as `libmtg-doomsday`; this crate depends on none of
//! them. Build a catalog with [`build_catalog`] and run a [`Scenario`] through
//! [`run_game`].
//!
//! Design docs (in this crate's `src/`):
//! - `DESIGN.org` — engine architecture and the IR primitive vocabulary.
//! - `CARD_INDEX.org` — how MTG mechanics decompose into IR primitives.

use rand::{Rng, SeedableRng};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

mod catalog;
pub(crate) use catalog::*;

mod card_defs;
pub use card_defs::build_catalog;

mod effects;
pub(crate) use effects::*;
pub use effects::Who;

mod predicates;
pub(crate) use predicates::*;
pub use predicates::TargetSpec;

mod snapshot;
mod objective;
pub use objective::Objective;
pub use snapshot::{
    BoardSnapshot, PlayerSnapshot, CardId, CardEntry, PermanentEntry,
    Stage, CardRegistry, SnapshotError,
    encode as snapshot_encode, decode as snapshot_decode,
    to_url_token, from_url_token,
};

// ── Public engine API for content crates (doomsday, ...) ────────────────────────
// Primitive state/decision vocabulary the concrete strategies read. These are
// projections of materialized state, not heuristics (those live in content).
pub use catalog::{
    CardDef, CardKind, ManaCost, Keyword, SourceZone, ActivationTiming, AbilityDef,
    ManaAbility, AddedMana, parse_mana_cost,
};
pub use predicates::{obj_matches, has_valid_target, pick_targets, legal_targets, is_protected_from};
pub use strategy::TargetGap;

mod strategy;
// Public decision API: the trait every player decision flows through, and the
// reusable do-nothing strategy (goldfish opponent / test stub).
pub use strategy::{Strategy, AlwaysPass, WishOption};

pub mod ir;

mod playable;

#[cfg(test)]
mod tests;

// ── Game state ────────────────────────────────────────────────────────────────

// ── Stable object identity ────────────────────────────────────────────────────

/// Opaque game object identifier. Every player, card, token, and stack ability
/// gets one at construction time and keeps it through all zone changes.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct ObjId(u64);

/// Type of a counter placed on a game object.
/// Counters persist across zone changes (stored on `GameObject`, not `BattlefieldState`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CounterType {
    /// Placed on cards exiled by Dauthi Voidwalker's replacement effect.
    Void,
    /// Placed on Engineered Explosives (and similar) via sunburst on entry.
    Charge,
    /// +1/+1 counter. Stored in `BattlefieldState.counters` for legacy
    /// compatibility; read by `fold_game_state_into_def`.
    PlusOnePlusOne,
    /// Loyalty counter (CR 306.5b/c): a planeswalker's loyalty *is* the number
    /// of these on it. Zone-scoped like `PlusOnePlusOne`, so it routes to the
    /// dedicated `BattlefieldState.loyalty` field rather than the cross-zone
    /// `GameObject.counters` map (auto-resets each battlefield stint).
    Loyalty,
    /// Stun counter (CR 122.1d): "if a permanent with a stun counter would
    /// become untapped, instead remove a stun counter." Zone-scoped, routes to
    /// `BattlefieldState.stun_counters` (e.g. Kaito −2).
    Stun,
    /// Lore counter (CR 714): a Saga's chapter abilities trigger as its lore
    /// count reaches their chapter number. Stored in the generic `counters` map.
    Lore,
    /// Burden counter (The One Ring). Stored in the generic cross-zone counter map.
    Burden,
}


impl ObjId {
    const UNSET: ObjId = ObjId(0);

}

/// Typed player identifier. Replaces the "us"/"opp" string convention.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PlayerId { Us, Opp }

impl PlayerId {
    pub fn opp(self) -> PlayerId {
        match self { PlayerId::Us => PlayerId::Opp, PlayerId::Opp => PlayerId::Us }
    }
}

impl std::fmt::Display for PlayerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { PlayerId::Us => write!(f, "us"), PlayerId::Opp => write!(f, "opp") }
    }
}

/// Zone a card currently occupies.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Zone {
    Library,
    Hand { known: bool },   // known = identity visible to opponent
    Stack,
    Battlefield,
    Graveyard,
    Exile { on_adventure: bool },
}

/// Context recording which objects moved during cost payment.
/// Carried on stack items so that resolution effects can inspect what was paid.
#[derive(Clone, Default)]
pub struct CostsPaidCtx {
    /// ObjIds of all objects moved as cost (exiled, discarded, sacrificed, returned).
    pub(crate) objects_moved: Vec<ObjId>,
    /// For each `ReturnFromBattlefield` payment, the `attack_target` the returned
    /// permanent had at the time it left the battlefield (in payment order).
    pub(crate) returned_attack_targets: Vec<Option<ObjId>>,
    /// Number of times a Replicate cost was paid for the spell (CR 702.58).
    /// Set during cost payment; used by cast_spell to push copies to the stack.
    pub(crate) replicate_count: u32,
    /// Strategy-chosen X value paid as `XLife` additional cost (0 if no X cost).
    pub(crate) chosen_x: u32,
    /// Chosen mode for modal spells (CR 700.2a). Set at cast time.
    pub(crate) chosen_mode: usize,
    /// Index of the alternate cost used from `def.alternate_costs()`, if any.
    /// `None` = hardcast (mana cost paid). Used by evoke triggers to detect
    /// whether the evoke cost path was taken (CR 702.74).
    pub(crate) alt_cost_index: Option<usize>,
}

/// Spell-on-stack state for a card while it's on the stack.
/// Populated at cast time; cleared when the spell resolves or is countered.
#[derive(Clone)]
pub struct SpellState {
    effect: Option<Effect>,
    pub chosen_targets: Vec<ObjId>,
    /// True when the back face of a split card was cast (e.g. an adventure instant).
    is_back_face: bool,
    /// Objects moved during cost payment (for effects that depend on what was paid).
    costs_paid_ctx: CostsPaidCtx,
}

/// In-play state for any permanent (land, creature, artifact, planeswalker, enchantment, token).
/// Replaces SimPermanent + SimLand. Whether a permanent is a land/creature/etc. is determined
/// by looking up its CardDef from the catalog.
#[derive(Clone)]
pub struct BattlefieldState {
    pub tapped: bool,
    damage: i32,
    pub entered_this_turn: bool,
    counters: i32,              // +1/+1 counters
    power_mod: i32,
    toughness_mod: i32,
    loyalty: i32,               // # of loyalty counters = the PW's loyalty (CR 306.5c); 0 for non-PWs. Routed to by CounterType::Loyalty.
    pub pw_activated_this_turn: bool,
    pub attacking: bool,
    pub unblocked: bool,
    pub attack_target: Option<ObjId>,  // None = attacking player, Some = attacking planeswalker
    /// Active face index for double-faced cards (0 = front, 1 = back). Flip sets this to 1.
    active_face: u8,
    /// Choice made as this permanent entered the battlefield (e.g. color for Painter's Servant,
    /// creature type for Cavern of Souls, card name for Disruptor Flute). Written by ETB
    /// replacement closures that call `resolve_choice`; cleared automatically on LTB when
    /// `bf` is dropped. Most cards also capture the choice value in their CE closure (the CE IS
    /// the primary storage); `etb_choice` is the side-channel for abilities that need to inspect
    /// "what was named" without holding a captured copy.
    pub(crate) etb_choice: Option<ChoiceResult>,
    /// Equipment: the creature this Equipment is attached to (CR 301.5).
    pub(crate) attached_to: Option<ObjId>,
    /// Stun counters (CR 122.1d): "If a permanent with a stun counter on it would become
    /// untapped, instead remove a stun counter from it."
    pub(crate) stun_counters: u32,
}

impl BattlefieldState {
    fn new() -> Self {
        BattlefieldState {
            tapped: false, damage: 0, entered_this_turn: true, counters: 0,
            power_mod: 0, toughness_mod: 0, loyalty: 0, pw_activated_this_turn: false,
            attacking: false, unblocked: false, attack_target: None,
            active_face: 0, etb_choice: None, attached_to: None, stun_counters: 0,
        }
    }
}

/// What kind of object this is — and, for objects that live in a zone, which
/// zone. The "kind" discriminant (card-in-a-zone, spell/ability on the stack, …)
/// that folds the old `zone`/`bf`/`spell`/`ability` fields into one enum so
/// illegal combinations (a permanent with no battlefield state, an ability in
/// the graveyard) are unrepresentable. `GameObject::zone()` derives the coarse
/// `Zone` from the variant. Players/emblems will join as their own variants —
/// kind is a separate concern from zone (a player has no zone at all).
pub enum ObjectRole {
    Library,
    Hand { known: bool },
    Battlefield(BattlefieldState),
    StackSpell(SpellState),
    StackAbility(AbilityState),
    Graveyard,
    Exile { on_adventure: bool },
    /// A player. Carries the player's whole game state; has no zone (CR: zones
    /// contain objects, players are objects-with-zones in our model). `PlayerState`
    /// holds a `Box<dyn Strategy>`, so neither this enum nor `GameObject` is `Clone`.
    Player(PlayerState),
}

// Not `Clone`: `ObjectRole::Player` holds a `Box<dyn Strategy>`. Nothing clones a
// whole GameObject (snapshots use their own types), so the derive was vestigial.
/// A card as a game object — follows the card through all zone changes.
/// Carries only game-accumulated state. The card's characteristics are derived
/// by looking up `catalog_key` in the catalog and applying continuous effects.
pub struct GameObject {
    pub id: ObjId,
    pub catalog_key: String,  // foreign key into the CardDef catalog
    pub owner: PlayerId,
    pub controller: PlayerId,
    pub is_token: bool,
    /// What kind of object this is + (for zone-resident kinds) which zone, with
    /// the per-kind state (battlefield / spell / ability) inline. See `ObjectRole`.
    role: ObjectRole,
    /// Inlined post-CE materialized snapshot. Rebuilt by `recompute` after each state-mutating tick.
    materialized: Option<CardDef>,
    /// Zone-independent counters (e.g. void counters from Dauthi Voidwalker).
    /// Persists across zone changes.
    pub(crate) counters: HashMap<CounterType, u32>,
    /// CI timestamp assigned when this object enters the battlefield.
    /// Used by `recompute` to give static-ability CIs stable timestamps across recompute cycles
    /// (CR 613.6: simultaneous effects from the same source share a timestamp).
    pub(crate) ci_timestamp: u32,
}

impl GameObject {
    /// Clone an object for a search branch. Player objects keep all game state
    /// but drop their strategy trait objects; every other role is cloned exactly.
    fn clone_for_search(&self) -> Self {
        let role = match &self.role {
            ObjectRole::Library => ObjectRole::Library,
            ObjectRole::Hand { known } => ObjectRole::Hand { known: *known },
            ObjectRole::Battlefield(bf) => ObjectRole::Battlefield(bf.clone()),
            ObjectRole::StackSpell(spell) => ObjectRole::StackSpell(spell.clone()),
            ObjectRole::StackAbility(ability) => ObjectRole::StackAbility(ability.clone()),
            ObjectRole::Graveyard => ObjectRole::Graveyard,
            ObjectRole::Exile { on_adventure } => ObjectRole::Exile { on_adventure: *on_adventure },
            ObjectRole::Player(ps) => ObjectRole::Player(ps.clone_for_search()),
        };
        Self {
            id: self.id,
            catalog_key: self.catalog_key.clone(),
            owner: self.owner,
            controller: self.controller,
            is_token: self.is_token,
            role,
            materialized: self.materialized.clone(),
            counters: self.counters.clone(),
            ci_timestamp: self.ci_timestamp,
        }
    }

    fn new(id: ObjId, catalog_key: impl Into<String>, owner: PlayerId) -> Self {
        GameObject {
            id, catalog_key: catalog_key.into(), controller: owner, owner,
            is_token: false, role: ObjectRole::Library, materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        }
    }

    /// Kind-first "is this object in zone `z`?" — matches on the role's *kind*
    /// (ignoring the `known`/`on_adventure` payload), so objects with no zone
    /// (a player, once that role exists) simply return `false`. Prefer this over
    /// `zone() == z` for membership tests; reserve `zone()` for the rare site
    /// that needs the actual zone value.
    fn in_zone(&self, z: Zone) -> bool {
        matches!((&self.role, z),
            (ObjectRole::Library, Zone::Library)
            | (ObjectRole::Hand { .. }, Zone::Hand { .. })
            | (ObjectRole::Battlefield(_), Zone::Battlefield)
            | (ObjectRole::StackSpell(_) | ObjectRole::StackAbility(_), Zone::Stack)
            | (ObjectRole::Graveyard, Zone::Graveyard)
            | (ObjectRole::Exile { .. }, Zone::Exile { .. }))
    }

    /// The coarse `Zone` this object occupies, derived from its role — `None` for
    /// an object with no zone (a player). Prefer `in_zone(z)` for membership tests.
    fn zone(&self) -> Option<Zone> {
        Some(match &self.role {
            ObjectRole::Library => Zone::Library,
            ObjectRole::Hand { known } => Zone::Hand { known: *known },
            ObjectRole::Battlefield(_) => Zone::Battlefield,
            ObjectRole::StackSpell(_) | ObjectRole::StackAbility(_) => Zone::Stack,
            ObjectRole::Graveyard => Zone::Graveyard,
            ObjectRole::Exile { on_adventure } => Zone::Exile { on_adventure: *on_adventure },
            ObjectRole::Player(_) => return None,
        })
    }

    /// The player game-state — `Some` only for a player object.
    fn player_state(&self) -> Option<&PlayerState> {
        if let ObjectRole::Player(ps) = &self.role { Some(ps) } else { None }
    }
    fn player_state_mut(&mut self) -> Option<&mut PlayerState> {
        if let ObjectRole::Player(ps) = &mut self.role { Some(ps) } else { None }
    }

    /// Battlefield state — `Some` only for a permanent.
    pub fn bf(&self) -> Option<&BattlefieldState> {
        if let ObjectRole::Battlefield(bf) = &self.role { Some(bf) } else { None }
    }
    fn bf_mut(&mut self) -> Option<&mut BattlefieldState> {
        if let ObjectRole::Battlefield(bf) = &mut self.role { Some(bf) } else { None }
    }

    /// Spell-on-stack state — `Some` only for a spell on the stack.
    pub fn spell(&self) -> Option<&SpellState> {
        if let ObjectRole::StackSpell(s) = &self.role { Some(s) } else { None }
    }
    fn spell_mut(&mut self) -> Option<&mut SpellState> {
        if let ObjectRole::StackSpell(s) = &mut self.role { Some(s) } else { None }
    }

    /// Ability-on-stack state — `Some` only for a card-less ability on the stack.
    /// Such an object has no `CardDef`, so `def_of`/`card_def_of` return None for it.
    fn ability(&self) -> Option<&AbilityState> {
        if let ObjectRole::StackAbility(a) = &self.role { Some(a) } else { None }
    }

    /// Transition to a payload-free zone (Library/Hand/Graveyard/Exile) or the
    /// Battlefield (preserving an existing `bf`, or starting a fresh one with
    /// `entered_this_turn`). Moving onto the Stack carries a spell/ability
    /// payload, so callers set `role` to `StackSpell`/`StackAbility` directly —
    /// this is a no-op for `Zone::Stack`.
    fn set_zone(&mut self, zone: Zone) {
        self.role = match zone {
            Zone::Library => ObjectRole::Library,
            Zone::Hand { known } => ObjectRole::Hand { known },
            Zone::Graveyard => ObjectRole::Graveyard,
            Zone::Exile { on_adventure } => ObjectRole::Exile { on_adventure },
            Zone::Battlefield => {
                let bf = match &self.role {
                    ObjectRole::Battlefield(bf) => bf.clone(),
                    _ => BattlefieldState { entered_this_turn: true, ..BattlefieldState::new() },
                };
                ObjectRole::Battlefield(bf)
            }
            Zone::Stack => return,
        };
    }
}


/// State carried by an *ability on the stack* — the card-less stack-object payload,
/// stored in `GameObject.ability`. An object with `ability.is_some()` is an ability
/// (CR 113.7 / 608): it has no card, so `def_of`/`card_def_of` return None for it.
/// The ability's controller and display name live on the `GameObject` itself
/// (`owner`/`controller` and `catalog_key`).
#[derive(Clone)]
pub struct AbilityState {
    pub(crate) effect: Effect,
    pub(crate) chosen_targets: Vec<ObjId>,
    pub(crate) costs_paid_ctx: CostsPaidCtx,
    /// True iff triggered (vs. activated). Projected by `Expr::AbilityIsTriggered`.
    pub(crate) is_triggered: bool,
    /// False iff "can't be countered" (CR 608.2b) — checked at resolution; the
    /// ability is still a legal target for counter effects.
    pub(crate) counterable: bool,
    /// If set, the engine enumerates choices at resolution and asks strategy to pick.
    pub(crate) choice_spec: Option<ChoiceSpec>,
}

// ── Trigger system ────────────────────────────────────────────────────────────

/// Zones a card or permanent can occupy.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ZoneId {
    Hand,
    Library,
    Battlefield,
    Graveyard,
    Exile,
    Stack,
}

/// A game event emitted at key moments. Handlers inspect this to decide whether their
/// trigger fires. Owned strings to avoid lifetime issues when pushing onto the stack.
#[derive(Clone)]
#[allow(dead_code)]
pub enum GameEvent {
    /// A card moved from one zone to another (ETB, GY→Exile, etc.).
    /// Does NOT include drawing — use `Draw` for that.
    ZoneChange {
        id: ObjId,
        actor: PlayerId,
        from: ZoneId,
        to: ZoneId,
        controller: PlayerId,
    },
    /// A player draws a card. `draw_index` is which draw this is this turn (1-based).
    /// `is_natural` is true only for the draw-step draw. `card` is the drawn card's id
    /// (the library top peeked at fire time), for naming it in the log.
    Draw {
        controller: PlayerId,
        draw_index: u8,
        is_natural: bool,
        card: Option<ObjId>,
    },
    /// Fired after step-specific actions complete and before priority begins.
    /// Only fires for named steps that have a priority round (not Untap or Cleanup).
    EnteredStep {
        step: StepKind,
        active_player: PlayerId,
    },
    /// Fired at the start of a phase-level priority window (main phases, which have no named steps).
    EnteredPhase {
        phase: PhaseKind,
    },
    /// Mana was added to a player's pool. Fires through the event pipeline so
    /// replacement effects (e.g. Damping Sphere) can intercept.
    ManaProduced { who: PlayerId, spec: String },
    /// A creature was declared as an attacker.
    CreatureAttacked {
        attacker_id: ObjId,
        attacker_controller: PlayerId,
    },
    /// Fired just before cost payment and state mutation in `cast_spell`. Prohibition gate.
    /// If suppressed by a "can't cast" prohibition, `cast_spell` returns `None`.
    SpellBeingCast {
        caster: PlayerId,
        card_id: ObjId,
        mana_value: i32,
        is_noncreature: bool,
    },
    /// Fired after all costs are paid and the spell object is on the stack.
    /// Used by triggers that react to casting (e.g. Lavinia's counter-free-spells trigger).
    SpellCast {
        caster: PlayerId,
        card_id: ObjId,
        mana_spent: bool,
        /// True if cast for one of its alternative costs (evoke, warp, flashback,
        /// free-cast …) rather than its mana cost. CR 118.9 / 702.74. Distinct
        /// from `!mana_spent` — a warp/flashback alt cost still spends mana.
        alt_cost: bool,
        /// The announced X paid for this cast (CR 601.2b), 0 if no X. Read back
        /// via `Ctx::ThisCast(EventField::X)` — e.g. Engineered Explosives'
        /// sunburst counters. The log is the durable home for cast-time choices.
        x: u32,
        /// Cards exiled to pay delve (CR 702.66), in payment order. Read via
        /// `Ctx::ThisCast(EventField::DelvedExiled)` — e.g. Murktide Regent
        /// counts the instant/sorcery cards among them.
        delved: Vec<ObjId>,
    },
    /// Fired after a spell finishes resolving — its effect has been applied (or it has
    /// become a permanent), just before priority returns. The general "spell resolved"
    /// event; objectives (and future resolution triggers) observe it.
    SpellResolved {
        controller: PlayerId,
        card_id: ObjId,
    },
    /// Fired inside `counter_one` before the counterable check, for spell objects only.
    /// Prohibition gate: "can't be countered" effects suppress this event (CR 614.17).
    /// `caster` is the controller of the spell being countered.
    SpellBeingCountered {
        caster: PlayerId,
        card_id: ObjId,
    },
    /// Fired in `sim_play_land` after the zone change. Distinguishes the once-per-turn
    /// land play from hand vs. lands entering via fetch, reanimate, etc.
    LandPlayed { id: ObjId, controller: PlayerId },
    /// A permanent transformed in place (CR 712.4). Fired by `Action::Transform`
    /// after the face flip so triggers can react ("whenever ~ transforms").
    Transformed { id: ObjId, controller: PlayerId },
    /// An Equipment/Aura became attached to a permanent (CR 702.6). Fired by
    /// `Action::Attach` so "whenever ~ becomes equipped/enchanted" triggers fire.
    /// `attachment` is the equipment/aura; `target` is the permanent it attached to.
    BecameAttached { attachment: ObjId, target: ObjId, controller: PlayerId },
    /// One token entered the battlefield (CR 111). Fired by `do_create_token`
    /// after the token object is inserted. `token_key` is its catalog key (e.g.
    /// "Fantasticar Construct"); objectives observe this to detect token-making
    /// wincons (The Fantasticar's pop).
    TokenCreated { controller: PlayerId, token_key: String, id: ObjId },
    /// A player lost `amount` (>0) life (CR 118.2) — from damage, payment, drain, …
    /// Logged centrally in `lose_life`. Read via `EventFilter::LifeLost` (e.g.
    /// Kaito 0 "draw for each opponent who lost life this turn").
    LifeLost { who: PlayerId, amount: i32 },
    // Future variants: DamageDealt, AbilityActivated, CounterChanged.
}

/// Data stored with a triggered ability waiting to be pushed onto the stack.
/// The effect closure captures all context (targets, source data) at trigger-push time.
#[derive(Clone)]
pub struct TriggerContext {
    /// Display name of the source — used for stack item naming and logging.
    pub(crate) source_name: String,
    /// Player who controls that permanent.
    pub(crate) controller: PlayerId,
    /// Legal targets this trigger may choose from. Resolved when pushed to the stack.
    pub(crate) target_spec: TargetSpec,
    /// The effect to apply when this trigger resolves. Receives the chosen targets.
    pub(crate) effect: Effect,
}

// ── Triggers and replacement effects ─────────────────────────────────────────

/// Signature for a per-card trigger check function.
/// Inspects the event and game state; if a trigger fires, appends a `TriggerContext` to `pending`.
pub type TriggerCheckFn =
    std::sync::Arc<dyn Fn(&GameEvent, ObjId, PlayerId, &SimState, &mut Vec<TriggerContext>) + Send + Sync>;

/// Signature for a per-card replacement check function.
/// Returns Some(targets) if this replacement applies to the event; None otherwise.
/// `source_id` is passed so self-ETB checks work without string dispatch.
pub type ReplacementCheckFn = std::sync::Arc<dyn Fn(&GameEvent, ObjId, PlayerId, &SimState) -> Option<Vec<ObjId>> + Send + Sync>;

/// Signature for a "can't happen" prohibition check (CR 614.17).
/// Returns true if the event is prohibited. Takes `&SimState` so checks can inspect card types,
/// controller, etc. Prohibition checks run before replacement effects (CR 614.17 — can't effects
/// aren't replacements and take precedence over permissive effects).
pub type ProhibitionCheckFn =
    std::sync::Arc<dyn Fn(&GameEvent, ObjId, PlayerId, &SimState) -> bool + Send + Sync>;

/// Predicate controlling when a card-bound trigger is armed.
/// Receives (source_id, &SimState) and returns true if the trigger should fire.
pub type TriggerPredicate =
    std::sync::Arc<dyn Fn(ObjId, &SimState) -> bool + Send + Sync>;

/// Default trigger predicate: source is on the battlefield.
/// Also correct for ETB-self triggers — fire_triggers runs at Stage 5, after
/// do_effect has already moved the card to the battlefield.
pub(crate) fn tp_on_battlefield() -> TriggerPredicate {
    Arc::new(|src, state| {
        state.objects.get(&src).map_or(false, |o| matches!(o.zone(), Some(Zone::Battlefield)))
    })
}

/// Trigger predicate: source is on the stack (e.g. Storm).
pub(crate) fn tp_on_stack() -> TriggerPredicate {
    Arc::new(|src, state| {
        state.objects.get(&src).map_or(false, |o| matches!(o.zone(), Some(Zone::Stack)))
    })
}

/// Predicate for LatentSpellMod: given (spell ObjId, caster, &SimState), returns
/// true if the spell qualifies for the latent modification.
pub type SpellPredicate =
    Arc<dyn Fn(ObjId, PlayerId, &SimState) -> bool + Send + Sync>;

/// A latent continuous effect that modifies the next qualifying spell cast
/// (CR 611.2f). Pushed by an ability resolution; consumed during 601.2a when
/// a qualifying spell is announced.
#[derive(Clone)]
pub struct LatentSpellMod {
    pub(crate) controller: PlayerId,
    /// Does this spell qualify? (e.g. "the next instant or sorcery spell")
    pub(crate) predicate: SpellPredicate,
    /// Given the qualifying spell's ObjId and controller, produce a CI to apply.
    pub(crate) make_ci: Arc<dyn Fn(ObjId, PlayerId) -> ContinuousInstance + Send + Sync>,
    /// Fallback expiry if no qualifying spell is cast.
    pub(crate) expiry: Expiry,
}

/// A trigger definition on a CardDef.  Pairs the check function with a predicate
/// that determines when the trigger is armed based on the source's game state.
#[derive(Clone)]
pub struct TriggerDef {
    pub(crate) check: TriggerCheckFn,
    /// In which state is this trigger armed?
    /// Default (tp_on_battlefield): source is on the battlefield.
    /// Storm: tp_on_stack (source is on the stack, fires on self-cast).
    pub(crate) active_when: TriggerPredicate,
}

/// Ephemeral trigger instance created at runtime by ability effects (e.g. Sneak Attack
/// end-step sacrifice, Tamiyo +2 watcher). Card-bound triggers are derived from catalog
/// at fire time via `fire_triggers`.
#[derive(Clone)]
pub struct TriggerInstance {
    pub(crate) source_id: ObjId,
    pub(crate) controller: PlayerId,
    pub(crate) check: TriggerCheckFn,
    /// None for permanent (card-based) triggers; Some for floating triggers created by abilities.
    pub(crate) expiry: Option<Expiry>,
}

/// Ephemeral replacement instance created at runtime by ability effects (e.g. Force of Negation
/// "exile instead of graveyard"). Card-bound replacements are derived from catalog at fire time.
#[derive(Clone)]
pub struct ReplacementInstance {
    pub(crate) source_id: ObjId,
    pub(crate) controller: PlayerId,
    pub(crate) check: ReplacementCheckFn,
    pub(crate) effect: Effect,
}

// ── Continuous effects (new model) ───────────────────────────────────────────

/// The five colors of Magic.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Color { White, Blue, Black, Red, Green }

/// A typed choice that an effect needs to make at resolution/ETB time.
/// Passed to `Strategy::resolve_choice` (reached per-player via `with_strategy`);
/// the strategy returns a `ChoiceResult`. This covers decisions that are not
/// targets (`TargetSpec`) and not object selections (`ChoiceSpec`) —
/// specifically, choices over abstract typed values.
#[derive(Clone)]
pub enum ChoiceRequest {
    /// Choose one of the five colors (e.g. Painter's Servant ETB).
    Color,
    /// Choose a creature type by name (e.g. Cavern of Souls ETB).
    CreatureType,
    /// Choose a card name (e.g. Disruptor Flute, Pithing Needle, Meddling Mage).
    CardName,
    /// Choose one of N modes for a modal spell (e.g. Sheoldred's Edict "Choose one —").
    /// The payload is the number of available modes. Strategy returns `ChoiceResult::Mode(i)`
    /// where `i < N`. Default: mode 0.
    Mode(usize),
    /// Offered when a Ward trigger resolves: should the targeting player pay the ward cost?
    /// Returns `ChoiceResult::Bool(true)` to pay (spell proceeds), `false` to decline (spell countered).
    WardPayment { cost: crate::ir::action::Action },
    /// "You may put one of these onto the battlefield" (CR 101.4, e.g. Show and Tell).
    /// Returns `ChoiceResult::OptionalObject(Some(id))` to place, or `None` to decline.
    MayPutOnBattlefield { candidates: Vec<ObjId> },
    /// "You may attach this Equipment to it" (CR 701.3).
    /// Returns `ChoiceResult::Bool(true)` to attach, `false` to decline.
    MayAttach,
}

/// The value returned by `Strategy::resolve_choice` for a given `ChoiceRequest`.
#[derive(Clone)]
pub enum ChoiceResult {
    Color(Color),
    CreatureType(String),
    CardName(String),
    Mode(usize),
    /// Returned for `ChoiceRequest::WardPayment`: true = pay, false = decline.
    Bool(bool),
    /// Returned for `ChoiceRequest::MayPutOnBattlefield`: chosen object or decline.
    OptionalObject(Option<ObjId>),
}

/// Card supertypes (Legendary, Basic, Snow, World, Ongoing).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Supertype { Legendary, Basic, Snow }

/// The seven layers in which continuous effects are applied (MTG rule 613).
/// Ordering is derived: effects in earlier layers apply before later ones.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[allow(dead_code)] // L1–L5 are defined for completeness; only L6–L7 are currently used
pub enum ContinuousLayer {
    L1CopyEffects      = 1,
    L2ControlEffects   = 2,
    L3TextEffects      = 3,
    L4TypeEffects      = 4,
    L5ColorEffects     = 5,
    L6AbilityEffects   = 6,
    L7PowerToughness   = 7,
}

/// Closure that mutates a cloned `CardDef` to apply a continuous effect modifier.
/// Receives `&SimState` so CDAs (characteristic-defining abilities) can read live game state.
pub type ContinuousModFn =
    std::sync::Arc<dyn Fn(&mut CardDef, &SimState) + Send + Sync>;

/// Predicate that decides whether a continuous effect applies to a given object.
/// Receives (target_id, target_controller, state).
pub type ContinuousFilterFn =
    std::sync::Arc<dyn Fn(ObjId, PlayerId, &SimState) -> bool + Send + Sync>;

/// What characteristics a CE reads from targets (for CR 613.7 dependency analysis).
/// If CE_A reads a category that CE_B writes, A depends on B within the same layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum CeReads {
    LandTypes,
    Supertypes,
    Abilities,
    Color,
    PowerToughness,
    CardTypes,
}

/// What characteristics a CE writes (modifies) on targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum CeWrites {
    LandTypes,
    Supertypes,
    Abilities,
    Color,
    PowerToughness,
    CardTypes,
}

/// When a `ContinuousInstance` expires and should be removed.
#[derive(Clone, PartialEq, Debug)]
pub enum Expiry {
    /// Removed during the Cleanup step of the current turn.
    EndOfTurn,
    /// Removed at the start of the controlling player's next Untap step.
    StartOfControllerNextTurn,
    /// Tied to a permanent being on the battlefield; removed when it leaves play.
    /// Used for ephemeral CEs created by abilities (e.g. Sneak Attack haste grant).
    WhileSourceOnBattlefield,
    /// Fires once, then self-removes. Used for delayed triggers (e.g. Sneak Attack
    /// "sacrifice at the beginning of the next end step").
    OneShot,
    /// Never expires. Used for emblems (CR 114).
    Never,
}

/// A single registered continuous-effect instance.
/// Created when a spell or ability that grants a CE resolves.
/// Removed when `expiry` is met.
#[derive(Clone)]
pub struct ContinuousInstance {
    /// Object that generated this effect (for expiry tracking and logging).
    pub(crate) source_id: ObjId,
    /// Controller of the source at the time the effect was created.
    pub(crate) controller: PlayerId,
    /// Which layer this modifier applies in (determines application order).
    pub(crate) layer: ContinuousLayer,
    /// CR 613.7: what this CE reads from targets to determine applicability/behavior.
    /// Used to compute dependency edges within a layer.
    pub(crate) reads: Vec<CeReads>,
    /// CR 613.7: what this CE writes (modifies) on targets.
    pub(crate) writes: Vec<CeWrites>,
    /// CR 613.6: registration sequence — tiebreaker after dependency ordering.
    pub(crate) timestamp: u32,
    /// Determines which game objects this CE affects.
    pub(crate) filter: ContinuousFilterFn,
    /// Mutates the target object's cloned `CardDef`.
    pub(crate) modifier: ContinuousModFn,
    /// When this instance should be removed.
    pub(crate) expiry: Expiry,
}

/// An emblem (CR 114): a command-zone marker with one or more static abilities,
/// controlled by the player whose effect created it, with no other characteristics.
/// Held in a side-list (like `continuous_instances`) rather than `state.objects`,
/// since it has no card type. Its abilities are gathered each `recompute`; it
/// never expires (CR 114.5 — an emblem can't leave the command zone).
#[derive(Clone)]
pub struct EmblemInstance {
    /// Identity (for `Ctx::Source` / logging); not present in `state.objects`.
    pub(crate) id: ObjId,
    pub(crate) controller: PlayerId,
    /// The emblem's static abilities (CR 114.5 — the emblem is their source).
    pub(crate) abilities: Vec<crate::ir::ability::Ability>,
    /// CR 613.6 timestamp, assigned at creation.
    pub(crate) timestamp: u32,
}


// ── Recompute ─────────────────────────────────────────────────────────────────

/// Apply counters and temporary P/T adjustments after continuous effects have
/// established the object's creature characteristics and base power/toughness.
fn fold_game_state_into_def(def: &mut CardDef, obj: &GameObject) {
    let Some(bf) = obj.bf() else { return };
    if let CardKind::Creature(c) = &mut def.kind {
        c.adjust_pt(bf.counters + bf.power_mod, bf.counters + bf.toughness_mod);
    }
}

/// CR 613.7: A depends on B if B writes a category that A reads.
fn ce_categories_match(r: CeReads, w: CeWrites) -> bool {
    matches!(
        (r, w),
        (CeReads::LandTypes, CeWrites::LandTypes)
            | (CeReads::Supertypes, CeWrites::Supertypes)
            | (CeReads::Abilities, CeWrites::Abilities)
            | (CeReads::Color, CeWrites::Color)
            | (CeReads::PowerToughness, CeWrites::PowerToughness)
            | (CeReads::CardTypes, CeWrites::CardTypes)
    )
}

/// Topological sort of CIs within a single layer using Kahn's algorithm.
/// Ties broken by timestamp (CR 613.6). Cycles fall back to timestamp order.
/// Topological sort within a single layer.
/// `static_cis` and `ephemeral_cis` form a combined index space:
/// indices 0..static_cis.len() refer to static_cis, the rest to ephemeral_cis.
fn topo_sort_layer(
    layer_slice: &[usize],
    static_cis: &[ContinuousInstance],
    ephemeral_cis: &[ContinuousInstance],
    out: &mut Vec<usize>,
) {
    use std::collections::BinaryHeap;
    use std::cmp::Reverse;

    let sc = static_cis.len();
    let get = |idx: usize| -> &ContinuousInstance {
        if idx < sc { &static_cis[idx] } else { &ephemeral_cis[idx - sc] }
    };

    let n = layer_slice.len();
    // Build dependency edges: in_degree[i] = count of CIs that i depends on.
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![vec![]; n];
    for i in 0..n {
        for j in 0..n {
            if i == j { continue; }
            let ci_i = get(layer_slice[i]);
            let ci_j = get(layer_slice[j]);
            if ci_i.reads.iter().any(|r| ci_j.writes.iter().any(|w| ce_categories_match(*r, *w))) {
                in_degree[i] += 1;
                dependents[j].push(i);
            }
        }
    }
    // Min-heap keyed by timestamp — independent CIs with lowest timestamp first.
    let mut ready: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();
    for i in 0..n {
        if in_degree[i] == 0 {
            ready.push(Reverse((get(layer_slice[i]).timestamp, i)));
        }
    }
    let start = out.len();
    while let Some(Reverse((_, idx))) = ready.pop() {
        out.push(layer_slice[idx]);
        for &dep in &dependents[idx] {
            in_degree[dep] -= 1;
            if in_degree[dep] == 0 {
                ready.push(Reverse((get(layer_slice[dep]).timestamp, dep)));
            }
        }
    }
    // Cycle fallback: append remaining in timestamp order.
    if out.len() - start < n {
        for i in 0..n {
            if !out[start..].contains(&layer_slice[i]) {
                out.push(layer_slice[i]);
            }
        }
    }
}

/// Rebuild each game object's inlined `materialized` field by applying all active
/// `ContinuousInstance`s to clones of the object's `CardDef` from the catalog.
///
/// All zones are covered: CEs such as Painter's Servant and Mycosynth Lattice can modify
/// per-card characteristics in every zone (hand, library, GY, exile, stack, battlefield).
/// Objects with no entry in the catalog (e.g. naked stack abilities) are silently skipped
/// by the `catalog.get()` guard below.
///
/// Called after every `fire_event` at recursion depth 0 (each "tick"). Strategy and display
/// code read `state.def_of(id)` which returns the inlined snapshot; they never access raw
/// `CardDef` fields directly.
///
/// Application order: CIs sorted by layer, then topologically by reads/writes
/// dependency within each layer, with timestamp as tiebreaker (CR 613.6/613.7).
/// CIs are applied **one at a time across all objects** (CE-by-CE, not object-by-object).
/// After each CI is applied, dependent CIs check whether their source still has the
/// generating ability via the in-progress materialized state (CR 613.7 dependency).
pub(crate) fn recompute(state: &mut SimState) {
    let ids: Vec<ObjId> = state.objects.keys().copied().collect();

    // Phase 1: initialize each object's materialized def from catalog base.
    for &id in &ids {
        let Some(catalog_key) = state.objects.get(&id).map(|o| o.catalog_key.clone()) else { continue };
        let Some(base) = state.catalog.get(&catalog_key) else { continue };
        let mut def = base.clone();

        // DFC back-face substitution: replace all printed characteristics with the
        // back face's values (CR 712.8a — the game sees only the face that's up).
        {
            let obj = state.objects.get(&id).unwrap();
            if obj.bf().map_or(false, |bf| bf.active_face == 1) {
                if let Some(ref back) = def.back.take() {
                    def.name = back.name.clone();
                    def.kind = back.kind.clone();
                    def.types = back.types.clone();
                    def.supertypes = back.supertypes.clone();
                    def.colors = back.colors.clone();
                }
            }
        }

        // Zone-based castable default: cards in hand are castable, others are not
        // (CEs may override — e.g. Dauthi sets castable=true on exiled cards).
        {
            let obj = state.objects.get(&id).unwrap();
            def.castable = obj.in_zone(Zone::Hand { known: false });
        }

        state.objects.get_mut(&id).unwrap().materialized = Some(def);
    }

    // Phase 1b: Generate static-ability CIs from catalog for all BF permanents.
    // These are produced fresh each recompute cycle with stable timestamps from
    // GameObject.ci_timestamp (assigned at ETB). They are NOT stored in
    // continuous_instances — only ephemeral CIs live there.
    let mut static_cis: Vec<ContinuousInstance> = Vec::new();
    for (&id, obj) in &state.objects {
        if !matches!(obj.zone(), Some(Zone::Battlefield)) { continue; }
        let Some(card_def) = state.catalog.get(&obj.catalog_key) else { continue };
        for factory in &card_def.static_ability_defs {
            let mut ci = factory(id, obj.controller);
            ci.timestamp = obj.ci_timestamp;
            static_cis.push(ci);
        }
        // IR-authored static abilities (dual-pathway alongside static_ability_defs).
        for ability in &card_def.abilities {
            for mut ci in crate::ir::executor::ir_static_to_cis(id, obj.controller, ability, state) {
                ci.timestamp = obj.ci_timestamp;
                static_cis.push(ci);
            }
        }
    }

    // Emblems (CR 114.5): their static abilities apply continuously from the
    // command zone, gathered the same way as battlefield static abilities.
    for emblem in &state.emblems {
        for ability in &emblem.abilities {
            for mut ci in crate::ir::executor::ir_static_to_cis(emblem.id, emblem.controller, ability, state) {
                ci.timestamp = emblem.timestamp;
                static_cis.push(ci);
            }
        }
    }

    // Build combined CI list: static-ability CIs + ephemeral CIs from state.
    // We index into this combined list for sorting and application.
    let static_count = static_cis.len();
    let total = static_count + state.continuous_instances.len();

    // Helper: access CI by combined index (0..static_count → static_cis, rest → ephemeral).
    let get_ci = |idx: usize| -> &ContinuousInstance {
        if idx < static_count { &static_cis[idx] }
        else { &state.continuous_instances[idx - static_count] }
    };

    // Phase 2: within each layer, compute dependency DAG and topological sort (CR 613.7).
    // A depends on B if B.writes overlaps A.reads. Ties broken by timestamp (CR 613.6).
    let mut ci_order: Vec<usize> = (0..total).collect();
    ci_order.sort_by_key(|&i| (get_ci(i).layer, get_ci(i).timestamp));
    let mut final_order: Vec<usize> = Vec::with_capacity(ci_order.len());
    let mut layer_start = 0;
    while layer_start < ci_order.len() {
        let current_layer = get_ci(ci_order[layer_start]).layer;
        let mut layer_end = layer_start;
        while layer_end < ci_order.len()
            && get_ci(ci_order[layer_end]).layer == current_layer
        {
            layer_end += 1;
        }
        let layer_slice = &ci_order[layer_start..layer_end];
        if layer_slice.len() <= 1 {
            final_order.extend_from_slice(layer_slice);
        } else {
            topo_sort_layer(layer_slice, &static_cis, &state.continuous_instances, &mut final_order);
        }
        layer_start = layer_end;
    }
    let ci_order = final_order;

    // Phase 3: apply CIs one at a time across all objects.
    for ci_idx in ci_order {
        let ci = get_ci(ci_idx);

        // CR 613.7: for static-ability CIs (idx < static_count), check whether the
        // source's in-progress materialized state still has static abilities.
        // If an earlier CI (e.g. Blood Moon) stripped them, this CI is suppressed.
        if ci_idx < static_count {
            let src = ci.source_id;
            let has_statics = |d: &CardDef| -> bool {
                !d.static_ability_defs.is_empty()
                    || d.abilities.iter().any(|a| {
                        matches!(a.kind, crate::ir::ability::AbilityKind::Static { .. })
                    })
            };
            let base_has_statics = state.objects.get(&src)
                .and_then(|o| state.catalog.get(&o.catalog_key))
                .map(has_statics)
                .unwrap_or(false);
            if base_has_statics {
                let suppressed = state.objects.get(&src)
                    .and_then(|o| o.materialized.as_ref())
                    .map(|d| !has_statics(d))
                    .unwrap_or(false);
                if suppressed { continue; }
            }
        }

        let modifier = std::sync::Arc::clone(&ci.modifier);
        let filter = std::sync::Arc::clone(&ci.filter);

        for &id in &ids {
            let controller = match state.objects.get(&id) {
                Some(o) => o.controller,
                None => continue,
            };
            if !filter(id, controller, state) { continue; }
            // Extract → modify → reinsert to avoid borrow conflict with &SimState.
            let mut def = match state.objects.get_mut(&id).and_then(|o| o.materialized.take()) {
                Some(d) => d,
                None => continue,
            };
            (modifier)(&mut def, state);
            state.objects.get_mut(&id).unwrap().materialized = Some(def);
        }
    }

    // A printed noncreature can now be a creature, and base-P/T setters must
    // not erase its counters. Rebuild from catalog each time and fold once.
    for &id in &ids {
        let obj = state.objects.get_mut(&id).unwrap();
        if let Some(mut def) = obj.materialized.take() {
            fold_game_state_into_def(&mut def, obj);
            obj.materialized = Some(def);
        }
    }

    // Phase 4: make granted abilities (CEMod::GrantAbility, layer 6) usable.
    // Run the same synthesis build_catalog uses for a card's own IR activated
    // abilities, now on the materialized `granted_abilities`, so a granted
    // ability is activatable exactly like a printed one. Mana-ness is *computed*
    // here (CR 605.1a via `is_mana_ability`), not pre-bucketed: each granted
    // activated ability lands in `mana_abilities` or `abilities` accordingly —
    // so both Urza's Saga chapter I (a mana ability) and chapter II (a non-mana
    // activated ability) fall out of the one `GrantAbility` primitive.
    for &id in &ids {
        let granted: Vec<crate::ir::ability::Ability> =
            match state.objects.get(&id).and_then(|o| o.materialized.as_ref()) {
                Some(def) if !def.granted_abilities.is_empty() => def.granted_abilities.clone(),
                _ => continue,
            };
        let synth_act: Vec<AbilityDef> = granted
            .iter()
            .filter_map(crate::ir::executor::ir_activated_as_legacy)
            .collect();
        let synth_mana: Vec<ManaAbility> = granted
            .iter()
            .filter_map(crate::ir::executor::ir_activated_as_mana_ability_legacy)
            .collect();
        if synth_act.is_empty() && synth_mana.is_empty() {
            continue;
        }
        if let Some(def) = state.objects.get_mut(&id).and_then(|o| o.materialized.as_mut()) {
            if !synth_act.is_empty() {
                if let Some(list) = def.abilities_vec_mut() {
                    list.extend(synth_act);
                }
            }
            if !synth_mana.is_empty() {
                if let Some(list) = def.mana_abilities_vec_mut() {
                    list.extend(synth_mana);
                }
            }
        }
    }
}

// ── Turn structure ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PhaseKind {
    Beginning,
    PreCombatMain,
    Combat,
    PostCombatMain,
    End,
}

#[derive(Clone, Copy, Debug)]
pub enum TurnPosition {
    Step(StepKind),
    Phase(PhaseKind),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum StepKind {
    Untap,
    Upkeep,
    Draw,
    BeginCombat,
    DeclareAttackers,
    DeclareBlockers,
    FirstStrikeCombatDamage,
    CombatDamage,
    EndCombat,
    End,
    Cleanup,
}

struct Step {
    kind: StepKind,
    prio: bool,
}

struct Phase {
    kind: PhaseKind,
    steps: Vec<Step>,
}

impl Phase {
    fn is_main_phase(&self) -> bool {
        matches!(self.kind, PhaseKind::PreCombatMain | PhaseKind::PostCombatMain)
    }
}

// ── Priority actions ──────────────────────────────────────────────────────────

/// Which face of a card to cast. `Back` = adventure/split second half.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SpellFace { Main, Back }

// ── Priority action types (CR 601.2 state machine) ──────────────────────────

/// Engine-provided legal action. Strategy picks one via `choose_action`.
#[derive(Clone, PartialEq, Debug)]
pub enum LegalAction {
    Pass,
    LandDrop(ObjId),
    /// Normal cast from hand, adventure back-face, or free cast from exile.
    CastSpell { card_id: ObjId, face: SpellFace },
    /// Activate a non-mana ability on a permanent.
    ActivateAbility { source_id: ObjId, ability_index: usize },
    /// Activate a mana ability with non-default timing (e.g. LED: instant-only).
    /// These are excluded from the CR 601.2g mana sub-loop but available during priority.
    ActivateManaAbility { source_id: ObjId, ability_index: usize },
}

/// Options presented to strategy at the Announce step (CR 601.2b).
pub struct AnnounceOptions {
    /// Legal modal choices for the spell at announcement time. Public so
    /// application-level agent adapters can expose only engine-approved modes.
    pub available_modes: Vec<usize>,
    pub available_alt_costs: Vec<AlternateCost>,
    pub has_x_cost: bool,
    /// Largest X that is currently payable on the normal cost path, after
    /// accounting for the spell's base mana component and X-mana/life costs.
    /// Strategies may choose a smaller value; choosing above this bound is rejected.
    pub max_x: u32,
}

/// Strategy's choices at the Announce step.
pub struct AnnounceChoice {
    pub chosen_mode: usize,
    /// Index into `AnnounceOptions.available_alt_costs` (= `def.alternate_costs()`).
    /// `None` = pay mana cost normally.
    pub alt_cost_index: Option<usize>,
    pub chosen_x: u32,
}

/// A mana ability the strategy can activate during ActivateMana.
#[derive(Clone)]
pub struct ManaAbilityOption {
    pub source_id: ObjId,
    pub ability_index: usize,
    produces: Vec<Color>,
    produces_count: usize,
}

/// Strategy's decision to activate a mana ability.
#[derive(Clone)]
pub struct ManaActivation {
    pub source_id: ObjId,
    pub ability_index: usize,
    /// Which color to produce (None = colorless/any, for generic mana needs).
    color_choice: Option<Color>,
}

// ── Phase constructors ────────────────────────────────────────────────────────

fn beginning_phase() -> Phase {
    Phase {
        kind: PhaseKind::Beginning,
        steps: vec![
            Step { kind: StepKind::Untap,  prio: false },
            Step { kind: StepKind::Upkeep, prio: true  },
            Step { kind: StepKind::Draw,   prio: true  },
        ],
    }
}

fn main_phase() -> Phase {
    Phase { kind: PhaseKind::PreCombatMain, steps: vec![] }
}

fn combat_phase() -> Phase {
    Phase {
        kind: PhaseKind::Combat,
        steps: vec![
            Step { kind: StepKind::BeginCombat,      prio: true },
            Step { kind: StepKind::DeclareAttackers, prio: true },
            Step { kind: StepKind::DeclareBlockers,  prio: true },
            Step { kind: StepKind::FirstStrikeCombatDamage, prio: true },
            Step { kind: StepKind::CombatDamage,     prio: true },
            Step { kind: StepKind::EndCombat,        prio: true },
        ],
    }
}

fn post_combat_main_phase() -> Phase {
    Phase { kind: PhaseKind::PostCombatMain, steps: vec![] }
}

fn end_phase() -> Phase {
    Phase {
        kind: PhaseKind::End,
        steps: vec![
            Step { kind: StepKind::End,     prio: true  },
            Step { kind: StepKind::Cleanup, prio: false },
        ],
    }
}

// ── Mana pool ─────────────────────────────────────────────────────────────────

/// Mana tracking: all 5 colors + colorless tracked separately; total covers all available mana.
#[derive(Clone, Default, Debug)]
pub struct ManaPool {
    pub w: i32,
    pub u: i32,
    pub b: i32,
    pub r: i32,
    pub g: i32,
    pub c: i32,
    pub total: i32,
}

impl ManaPool {
    pub fn can_pay(&self, cost: &ManaCost) -> bool {
        self.w >= cost.w && self.u >= cost.u && self.b >= cost.b &&
        self.r >= cost.r && self.g >= cost.g && self.c >= cost.c &&
        self.total >= cost.total_specific() + cost.generic
    }

    pub fn spend(&mut self, cost: &ManaCost) {
        self.w -= cost.w;
        self.u -= cost.u;
        self.b -= cost.b;
        self.r -= cost.r;
        self.g -= cost.g;
        self.c -= cost.c;
        self.total -= cost.total_specific() + cost.generic;
        // Generic costs may consume colored mana; reduce excess colored counters
        // proportionally so the invariant total >= sum_of_specifics holds.
        let color_sum = self.w + self.u + self.b + self.r + self.g + self.c;
        let excess = color_sum.saturating_sub(self.total);
        if excess > 0 {
            // Drain colors in priority: b, u, w, r, g, c
            let mut remaining = excess;
            for field in [&mut self.b, &mut self.u, &mut self.w, &mut self.r, &mut self.g, &mut self.c] {
                let drain = remaining.min(*field);
                *field -= drain;
                remaining -= drain;
                if remaining == 0 { break; }
            }
        }
    }

    pub fn add(&mut self, other: &ManaPool) {
        self.w += other.w;
        self.u += other.u;
        self.b += other.b;
        self.r += other.r;
        self.g += other.g;
        self.c += other.c;
        self.total += other.total;
    }

    fn drain(&mut self) {
        *self = ManaPool::default();
    }
}

/// Apply continuous casting-cost changes to the mana component of a spell cost.
/// `casting_cost_modifier` is applied first, then the Trinisphere-style floor.
pub(crate) fn apply_casting_cost_rules(mut cost: ManaCost, def: &CardDef) -> ManaCost {
    cost.generic = (cost.generic + def.casting_cost_modifier).max(0);
    let floor = def.minimum_cast_mana.max(0);
    let current = cost.mana_value();
    if current < floor {
        cost.generic += floor - current;
    }
    cost
}

/// Add the announced X-mana component to a spell's already-adjusted base mana cost.
/// XLife-only spells leave the mana component unchanged.
pub(crate) fn combined_mana_cost_with_x(
    mut base: ManaCost,
    additional: &crate::ir::ability::CostBody,
    chosen_x: u32,
) -> ManaCost {
    if additional.has_x_mana_cost() {
        base.generic = base.generic.saturating_add(chosen_x.min(i32::MAX as u32) as i32);
    }
    base
}

/// Largest X currently payable on the normal mana-cost path.  This is used at
/// announcement time so strategies never default to an impossible X.  For XMana
/// we test the *combined* base+X mana cost against potential mana; for XLife we
/// also respect the existing strategic safeguard of leaving at least one life.
pub(crate) fn max_affordable_x_for_spell(
    state: &SimState,
    who: PlayerId,
    def: &CardDef,
    face: SpellFace,
) -> u32 {
    let additional = &def.additional_costs;
    if !additional.has_x_cost() {
        return 0;
    }

    let mut max_x = u32::MAX;

    if additional.has_x_mana_cost() {
        let base = match face {
            SpellFace::Main => apply_casting_cost_rules(parse_mana_cost(def.mana_cost()), def),
            SpellFace::Back => {
                let raw = def.adventure().map(|a| a.mana_cost()).unwrap_or("");
                apply_casting_cost_rules(parse_mana_cost(raw), def)
            }
        };
        let potential = state.potential_mana(who);
        let ceiling = potential.total.max(0) as u32;
        let mut affordable = 0u32;
        for x in 0..=ceiling {
            let combined = combined_mana_cost_with_x(base.clone(), additional, x);
            if potential.can_pay(&combined) {
                affordable = x;
            } else if x > affordable {
                break;
            }
        }
        max_x = max_x.min(affordable);
    }

    if additional.has_x_life_cost() {
        max_x = max_x.min(state.player(who).life.saturating_sub(1).max(0) as u32);
    }

    if max_x == u32::MAX { 0 } else { max_x }
}

// ── Mana ability primitives ───────────────────────────────────────────────────

/// Enumerate all mana abilities that `who` can currently activate.
/// Checks zone, tapped state, activatable flag, and condition predicate.
pub(crate) fn enumerate_mana_abilities(state: &SimState, who: PlayerId) -> Vec<ManaAbilityOption> {
    let mut options = Vec::new();
    // Battlefield permanents.
    for card in state.permanents_of(who) {
        // Null Rod / Karn: artifact mana abilities can't be activated. Source-keyed
        // action-Restriction (mana abilities are activated abilities — CR 605.1a).
        if crate::ir::executor::mana_ability_restricted(state, card.id) { continue; }
        let mas = state.def_of(card.id).map(|d| d.mana_abilities()).unwrap_or(&[]);
        let bf = match card.bf() { Some(bf) => bf, None => continue };
        for (idx, ma) in mas.iter().enumerate() {
            if !ma.activatable { continue; }
            if ma.timing != ActivationTiming::Default { continue; } // non-default timing excluded from mana sub-loop
            if !matches!(ma.source_zone, SourceZone::Battlefield) { continue; }
            if ma_requires_tap(ma) && bf.tapped { continue; }
            if ma.condition.as_ref().map_or(false, |cond| !obj_matches(cond, card.id, state)) { continue; }
            // Paid mana abilities (for example Giant's Boulder: {1}, {T}: add one mana)
            // may not recursively pay for themselves.  Only expose one to the mana
            // sub-loop once its mana component is already payable from the current
            // pool. Free mana sources remain available as usual, so the loop can
            // activate those first and make a filter available on a later iteration.
            if let Some(mc) = ma.costs.first_mana_cost() {
                if mc.mana_value() > 0 && !state.player(who).pool.can_pay(&mc) {
                    continue;
                }
            }
            options.push(ManaAbilityOption {
                source_id: card.id,
                ability_index: idx,
                produces: ma.produces.clone(),
                produces_count: ma.produces_count,
            });
        }
    }
    // Hand-zone mana abilities (e.g. Simian Spirit Guide).
    for card in state.hand_of(who) {
        // The spell being cast is reserved for the stack, not a hand mana source.
        if state.casting_spell == Some(card.id) { continue; }
        if crate::ir::executor::mana_ability_restricted(state, card.id) { continue; }
        let mas = state.catalog.get(&card.catalog_key).map(|d| d.mana_abilities()).unwrap_or(&[]);
        for (idx, ma) in mas.iter().enumerate() {
            if !ma.activatable { continue; }
            if !matches!(ma.source_zone, SourceZone::Hand) { continue; }
            if let Some(mc) = ma.costs.first_mana_cost() {
                if mc.mana_value() > 0 && !state.player(who).pool.can_pay(&mc) {
                    continue;
                }
            }
            options.push(ManaAbilityOption {
                source_id: card.id,
                ability_index: idx,
                produces: ma.produces.clone(),
                produces_count: ma.produces_count,
            });
        }
    }
    options
}

/// Compute a tap plan for paying `cost` without mutating state.
/// Returns ManaActivations in order: specific colors first (B, U, W, R, G), then generic.
pub fn auto_tap_plan(state: &SimState, who: PlayerId, cost: &ManaCost) -> Vec<ManaActivation> {
    // Public/full-cost entry point: subtract mana already floating, then plan
    // only for the residual demand.  The cost executor itself already computes
    // a residual ManaShortage before entering the CR 601.2g mana loop; that
    // path must call `auto_tap_plan_remaining` directly or the floating pool
    // would be subtracted twice.
    let pool = &state.player(who).pool;
    let remaining = ManaCost {
        w: (cost.w - pool.w).max(0),
        u: (cost.u - pool.u).max(0),
        b: (cost.b - pool.b).max(0),
        r: (cost.r - pool.r).max(0),
        g: (cost.g - pool.g).max(0),
        c: (cost.c - pool.c).max(0),
        generic: (cost.generic - (pool.total
            - pool.w.min(cost.w) - pool.u.min(cost.u) - pool.b.min(cost.b)
            - pool.r.min(cost.r) - pool.g.min(cost.g) - pool.c.min(cost.c)
        ).max(0)).max(0),
    };
    auto_tap_plan_remaining(state, who, &remaining)
}

/// Plan mana activations for a cost that is *already* the unpaid residual.
///
/// `cost_exec::PayError::ManaShortage` uses this representation.  Do not
/// subtract the current floating pool again here: it has already been applied
/// when the residual was computed.  Keeping this separate from
/// `auto_tap_plan` prevents legal 3/4-mana casts from stalling after a first
/// source produces two or three mana.
pub fn auto_tap_plan_remaining(state: &SimState, who: PlayerId, cost: &ManaCost) -> Vec<ManaActivation> {

    let mut plan = Vec::new();
    let mut used: HashSet<ObjId> = HashSet::new();
    // The action generator and the mana loop share this list. Planning from the
    // same snapshot keeps an unfunded paid mode out of the plan entirely.
    let available: HashSet<(ObjId, usize)> = enumerate_mana_abilities(state, who)
        .into_iter().map(|a| (a.source_id, a.ability_index)).collect();

    // Helper: find a battlefield source producing `color` (or any if None).
    let find_bf = |state: &SimState, used: &HashSet<ObjId>, color: Option<Color>, prefer_large: bool| -> Option<(ObjId, usize, usize)> {
        let sources = state.objects.iter().filter_map(|(id, c)| {
            if used.contains(id) { return None; }
            if c.controller != who || !c.in_zone(Zone::Battlefield) { return None; }
            // Null Rod / Karn: don't plan to tap an artifact whose abilities are restricted.
            if crate::ir::executor::mana_ability_restricted(state, *id) { return None; }
            let bf = c.bf()?;
            let mas = state.def_of(*id).map(|d| d.mana_abilities()).unwrap_or(&[]);
            let need_for = |c: Color| -> i32 {
                match c {
                    Color::White => cost.w,
                    Color::Blue => cost.u,
                    Color::Black => cost.b,
                    Color::Red => cost.r,
                    Color::Green => cost.g,
                }
            };
            // Prefer a multi-mana mode that covers as many currently-required
            // colored pips as possible. This matters for sources like Relic of
            // Sauron, which may produce UB/UR/BR in one activation rather than
            // two copies of a single color.
            let coverage = |ma: &ManaAbility| -> i32 {
                let mut score = 0;
                for c in [Color::White, Color::Blue, Color::Black, Color::Red, Color::Green] {
                    let produced = ma.produces.iter().filter(|&&x| x == c).count() as i32;
                    score += produced.min(need_for(c));
                }
                score
            };
            let (idx, ma) = mas.iter().enumerate()
                .filter(|(idx, ma)| {
                    available.contains(&(*id, *idx))
                        && ma.activatable
                        && ma.timing == ActivationTiming::Default // exclude LED, instant-only abilities
                        && matches!(ma.source_zone, SourceZone::Battlefield)
                        && (!ma_requires_tap(ma) || !bf.tapped)
                        && ma.condition.as_ref().map_or(true, |cond| obj_matches(cond, *id, state))
                        && ma.costs.first_mana_cost().map_or(true, |mc| state.player(who).pool.can_pay(&mc))
                        // A paid filter (e.g. Giant's Boulder: pay {1}, add one)
                        // cannot fill a generic shortfall: it adds no net mana.
                        && (color.is_some() || ma.costs.first_mana_cost()
                            .map_or(true, |mc| ma.produces_count as i32 > mc.mana_value()))
                        && color.map_or(true, |c| ma.produces.contains(&c))
                })
                .max_by_key(|(_, ma)| (coverage(ma), ma.produces_count))?;
            Some((*id, idx, ma.produces_count))
        });
        sources.max_by_key(|(id, idx, count)| {
            let ma = &state.def_of(*id).unwrap().mana_abilities()[*idx];
            // Tap sources such as Mox Opal before sacrificing artifacts that
            // can turn off metalcraft. Break ties identically in live/forked states.
            (!ma.costs.requires_sac_self(), if prefer_large { *count } else { 0 },
                std::cmp::Reverse(*id))
        })
    };

    // Specific colorless pips ({C}) are not generic: they require a source
    // that actually produces colorless mana.  The older planner skipped this
    // requirement entirely, which let XCC spells reach payment with no {C}.
    let find_bf_colorless = |state: &SimState, used: &HashSet<ObjId>| -> Option<(ObjId, usize, usize)> {
        state.objects.iter().find_map(|(id, c)| {
            if used.contains(id) { return None; }
            if c.controller != who || !c.in_zone(Zone::Battlefield) { return None; }
            if crate::ir::executor::mana_ability_restricted(state, *id) { return None; }
            let bf = c.bf()?;
            let mas = state.def_of(*id).map(|d| d.mana_abilities()).unwrap_or(&[]);
            let (idx, ma) = mas.iter().enumerate()
                .filter(|(idx, ma)| {
                    available.contains(&(*id, *idx))
                        && ma.activatable
                        && ma.timing == ActivationTiming::Default
                        && matches!(ma.source_zone, SourceZone::Battlefield)
                        && (!ma_requires_tap(ma) || !bf.tapped)
                        && ma.condition.as_ref().map_or(true, |cond| obj_matches(cond, *id, state))
                        && ma.costs.first_mana_cost().map_or(true, |mc| state.player(who).pool.can_pay(&mc))
                        && ma.costs.first_mana_cost()
                            .map_or(true, |mc| ma.produces_count as i32 > mc.mana_value())
                        // Colorless producers encode no W/U/B/R/G entries.
                        && ma.produces.is_empty()
                        && ma.produces_count > 0
                })
                .max_by_key(|(_, ma)| ma.produces_count)?;
            Some((*id, idx, ma.produces_count))
        })
    };

    let find_hand = |state: &SimState, used: &HashSet<ObjId>, color: Option<Color>| -> Option<(ObjId, usize)> {
        state.hand_of(who).find_map(|c| {
            if used.contains(&c.id) { return None; }
            if crate::ir::executor::mana_ability_restricted(state, c.id) { return None; }
            let mas = state.catalog.get(&c.catalog_key).map(|d| d.mana_abilities()).unwrap_or(&[]);
            let (idx, _) = mas.iter().enumerate().find(|(idx, ma)| {
                available.contains(&(c.id, *idx))
                    && ma.activatable
                    && matches!(ma.source_zone, SourceZone::Hand)
                    && ma.costs.first_mana_cost().map_or(true, |mc| state.player(who).pool.can_pay(&mc))
                    && (color.is_some() || ma.costs.first_mana_cost()
                        .map_or(true, |mc| ma.produces_count as i32 > mc.mana_value()))
                    && color.map_or(true, |col| ma.produces.contains(&col))
            })?;
            Some((c.id, idx))
        })
    };

    // Specific colors first.
    for &(need, color) in &[
        (cost.b, Color::Black), (cost.u, Color::Blue), (cost.w, Color::White),
        (cost.r, Color::Red), (cost.g, Color::Green),
    ] {
        let mut remaining = need;
        while remaining > 0 {
            if let Some((id, idx, _)) = find_bf(state, &used, Some(color), false) {
                plan.push(ManaActivation { source_id: id, ability_index: idx, color_choice: Some(color) });
                used.insert(id);
                remaining -= 1;
            } else if let Some((id, idx)) = find_hand(state, &used, Some(color)) {
                plan.push(ManaActivation { source_id: id, ability_index: idx, color_choice: Some(color) });
                used.insert(id);
                remaining -= 1;
            } else if let Some((id, idx, _)) = find_bf(state, &used, None, true) {
                // A colored mana filter may require generic mana to activate.
                // Produce that mana first; the caller replans after this one
                // activation and can then select the newly affordable filter.
                plan.push(ManaActivation { source_id: id, ability_index: idx, color_choice: None });
                used.insert(id);
                break;
            } else if let Some((id, idx)) = find_hand(state, &used, None) {
                plan.push(ManaActivation { source_id: id, ability_index: idx, color_choice: None });
                used.insert(id);
                break;
            } else {
                break;
            }
        }
    }

    // Specific colorless mana ({C}).
    let mut remaining_colorless = cost.c;
    while remaining_colorless > 0 {
        if let Some((id, idx, count)) = find_bf_colorless(state, &used) {
            plan.push(ManaActivation { source_id: id, ability_index: idx, color_choice: None });
            used.insert(id);
            remaining_colorless -= count as i32;
        } else {
            break;
        }
    }

    // Generic mana.
    let mut remaining_generic = cost.generic;
    while remaining_generic > 0 {
        if let Some((id, idx, count)) = find_bf(state, &used, None, false) {
            plan.push(ManaActivation { source_id: id, ability_index: idx, color_choice: None });
            used.insert(id);
            remaining_generic -= count as i32;
        } else if let Some((id, idx)) = find_hand(state, &used, None) {
            plan.push(ManaActivation { source_id: id, ability_index: idx, color_choice: None });
            used.insert(id);
            remaining_generic -= 1;
        } else {
            break;
        }
    }

    plan
}

/// Execute a single mana ability activation: pay costs via the general path, resolve
/// effect immediately (mana abilities don't use the stack — CR 605.3b).
fn execute_mana_activation(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    act: &ManaActivation,
) -> bool {
    if state.casting_spell == Some(act.source_id) { return false; }
    let ma = state.def_of(act.source_id)
        .and_then(|d| d.mana_abilities().get(act.ability_index).cloned())
        .or_else(|| {
            let key = &state.objects.get(&act.source_id)?.catalog_key;
            state.catalog.get(key.as_str())?.mana_abilities().get(act.ability_index).cloned()
        });
    let Some(ma) = ma else { return false; };

    // Pay costs via the general cost path.  A failed paid mana ability must
    // NOT still resolve its AddMana body; doing so would create mana for free
    // and make legality prediction diverge from execution.
    let crate::ir::ability::CostBody::Ir(action) = &ma.costs;
    if pay_ir_cost(state, t, who, act.source_id, action, false).is_none() {
        return false;
    }

    // Resolve effect immediately — mana production, logging via ManaProduced event.
    ma.make_effect.clone()(who, act.color_choice).call(state, t, &[]);
    true
}

/// Activate at most one mana source for an already-computed unpaid residual.
///
/// `remaining` is the exact shortfall returned by `remaining_mana` /
/// `ManaShortage`; the strategy must not subtract the floating pool again.
fn activate_one_mana_source_for_remaining(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    remaining: &ManaCost,
) -> bool {
    let available = enumerate_mana_abilities(state, who);
    if available.is_empty() {
        return false;
    }
    let activation = state.with_strategy(who, |s, st| {
        s.choose_mana_ability(st, who, &available, remaining)
    });
    let Some(act) = activation else {
        return false;
    };
    execute_mana_activation(state, t, who, &act)
}

/// Strategy-driven mana ability loop (CR 601.2g) for a full mana cost.
///
/// Before every activation we recompute the exact residual from the current
/// floating pool and pass that residual to the strategy.  This avoids the old
/// double-subtraction bug where a 3-mana spell with 2 already floating was
/// told that its remaining {1} had already been paid.
fn run_mana_loop(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    mana_cost: &ManaCost,
) {
    let mut activations = 0usize;
    loop {
        if state.player(who).pool.can_pay(mana_cost) {
            break;
        }
        // Defensive guard for malformed/custom mana abilities or a strategy that
        // keeps choosing a zero-progress activation. Real deck positions should
        // exhaust available tap/sacrifice sources long before this threshold.
        if activations >= 64 {
            eprintln!("[mana] aborting mana loop after 64 activations (possible zero-progress cycle)");
            break;
        }
        let remaining = crate::ir::executor::remaining_mana(&state.player(who).pool, mana_cost);
        if !activate_one_mana_source_for_remaining(state, t, who, &remaining) {
            break;
        }
        activations += 1;
    }
}

/// Check what the built-in mana pilot can actually pay, using an isolated game
/// state so the estimate cannot tap lands, sacrifice cards, or spend life in the
/// live game. The pooled color capabilities in `potential_mana` alone are not
/// enough: a paid filter can consume a mana that estimate already counted.
pub(crate) fn can_pay_with_auto_mana(
    state: &SimState, who: PlayerId, cost: &ManaCost, reserve_tap_source: Option<ObjId>,
) -> bool {
    can_pay_with_auto_mana_context(state, who, cost, reserve_tap_source, state.casting_spell)
}

/// Reserve the announced spell while predicting its mana payment, just as the
/// real cast path does. A Spirit Guide cannot be exiled to pay for itself.
pub(crate) fn can_pay_spell_with_auto_mana(
    state: &SimState, who: PlayerId, cost: &ManaCost, card_id: ObjId,
) -> bool {
    can_pay_with_auto_mana_context(state, who, cost, None, Some(card_id))
}

fn can_pay_with_auto_mana_context(
    state: &SimState, who: PlayerId, cost: &ManaCost,
    reserve_tap_source: Option<ObjId>, casting_spell: Option<ObjId>,
) -> bool {
    if state.player(who).pool.can_pay(cost) { return true; }
    let mut trial = state.fork_for_search(0);
    trial.casting_spell = casting_spell;
    if !trial.potential_mana(who).can_pay(cost) { return false; }
    trial.set_strategy(who, Box::new(crate::strategy::AlwaysPass::new(who)));
    if let Some(source) = reserve_tap_source {
        if let Some(bf) = trial.permanent_bf_mut(source) { bf.tapped = true; }
    }
    let turn = trial.current_turn as u8;
    run_mana_loop(&mut trial, turn, who, cost);
    trial.player(who).pool.can_pay(cost)
}

/// Phase 3 cost-IR: pay an `Action`-shaped cost.
///
/// 1. Build the schema via `cost_exec::build_schema`. `None` here means the
///    cost is structurally unpayable (e.g. Sacrifice with insufficient
///    targets) — bail.
/// 2. Ask the strategy for a `BindEnv` answering every Decision. With no
///    strategy available, fall back to `default_announcement`.
/// 3. Run the cost via `cost_exec::pay`. On `ManaShortage`, drive the
///    mana sub-loop (CR 601.2g) and retry. The retry cap prevents an
///    infinite loop when the strategy keeps producing no progress.
///
/// Returns `None` on any unrecoverable failure (invalid bindings, mana
/// shortage that can't be satisfied, structural unpayability).
#[allow(dead_code)]
pub(crate) fn pay_ir_cost(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    source: ObjId,
    action: &crate::ir::action::Action,
    consult_strategy: bool,
) -> Option<CostsPaidCtx> {
    let schema = crate::ir::cost_exec::build_schema(action, state, who, source)?;
    // Acquire the payer's strategy per-decision (never held across execution) so
    // nested cost decisions in cost_exec/execute_mut can re-acquire it too.
    let env = if consult_strategy {
        state.with_strategy(who, |s, st| s.propose_announcement(st, source, &schema))
    } else {
        crate::strategy::default_announcement(&schema)
    };
    for _attempt in 0..64 {
        match crate::ir::cost_exec::pay(action, &schema, &env, state, t, who, source) {
            Ok(ctx) => return Some(ctx),
            Err(crate::ir::cost::PayError::ManaShortage(rem)) => {
                // `rem` is already the unpaid residual after the current pool
                // was applied. Activate one source against that residual, then
                // retry the cost executor so it recomputes the next shortage
                // from the real post-activation pool.
                if !activate_one_mana_source_for_remaining(state, t, who, &rem) {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    None
}

/// Bind the announced X (`chosen_x`) into `env` under `$x`, overriding whatever
/// the strategy proposed. Additional-cost XLife/XMana payments read this — the
/// value is announced once at CR 601.2b and shared with the spell's resolution
/// effect, so the cost layer consumes it rather than re-deciding it. Harmless
/// for costs with no `$x` decision (the extra binding is ignored).
fn bind_announced_x(env: &mut crate::ir::executor::BindEnv, chosen_x: u32) {
    env.bindings.insert("$x", crate::ir::expr::Value::Num(chosen_x as i64));
}

/// Feasibility of a spell's additional IR cost (CR 118.9d) given the announced
/// `chosen_x`. Replaces the legacy `can_pay_costs(&def.additional_costs, …)`.
/// Returns true iff the cost is structurally payable and the announced X is in
/// range (e.g. enough potential mana for XMana, enough life for XLife).
pub(crate) fn can_pay_additional_ir_cost(
    state: &SimState,
    who: PlayerId,
    source: ObjId,
    cost: &crate::ir::ability::CostBody,
    chosen_x: u32,
) -> bool {
    let crate::ir::ability::CostBody::Ir(action) = cost;
    let Some(schema) = crate::ir::cost_exec::build_schema(action, state, who, source) else {
        return false;
    };
    let mut env = crate::strategy::default_announcement(&schema);
    bind_announced_x(&mut env, chosen_x);
    crate::ir::cost_exec::validate_env(&schema, &env)
}

/// Pay a spell's additional IR cost (CR 118.9d). Like `pay_ir_cost` but
/// pre-binds the announced `chosen_x` so XLife/XMana pay exactly the value
/// shared with the resolution effect. Records `chosen_x` in the returned
/// `CostsPaidCtx`.
fn pay_additional_ir_cost(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    source: ObjId,
    cost: &crate::ir::ability::CostBody,
    chosen_x: u32,
    consult_strategy: bool,
) -> Option<CostsPaidCtx> {
    let crate::ir::ability::CostBody::Ir(action) = cost;
    let schema = crate::ir::cost_exec::build_schema(action, state, who, source)?;
    let mut env = if consult_strategy {
        state.with_strategy(who, |s, st| s.propose_announcement(st, source, &schema))
    } else {
        crate::strategy::default_announcement(&schema)
    };
    bind_announced_x(&mut env, chosen_x);
    for _attempt in 0..64 {
        match crate::ir::cost_exec::pay(action, &schema, &env, state, t, who, source) {
            Ok(mut ctx) => {
                ctx.chosen_x = chosen_x;
                return Some(ctx);
            }
            Err(crate::ir::cost::PayError::ManaShortage(rem)) => {
                // Same residual-cost protocol as `pay_ir_cost`: one activation
                // per retry prevents the floating pool from being applied twice.
                if !activate_one_mana_source_for_remaining(state, t, who, &rem) {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    None
}

// ── Fetch land detection ─────────────────────────────────────────────────────

// ── Mana potential accumulation ───────────────────────────────────────────────

/// Accumulate one source's potential contribution into the pool.
/// A source (land or permanent) contributes at most 1 to `total` because a single
/// tap or sacrifice produces one mana. The per-color fields reflect which colors
/// that source *can* produce (union across all available abilities).
fn ma_requires_tap(ma: &ManaAbility) -> bool {
    ma.costs.requires_tap_self()
}

pub fn accumulate_source_potential(abilities: &[ManaAbility], tapped: bool, p: &mut ManaPool) {
    let avail: Vec<_> = abilities.iter()
        .filter(|ma| !ma_requires_tap(ma) || !tapped)
        .collect();
    if avail.is_empty() { return; }
    let count = avail.iter().map(|ma| ma.produces_count).max().unwrap_or(1) as i32;
    p.total += count;
    // Track which colors this source *can* produce (union across available abilities).
    // Scale by produces_count so e.g. Ancient Tomb registers 2 colorless.
    let mut produced = [false; 5]; // W U B R G
    let mut any_color = false;
    for ma in &avail {
        for color in &ma.produces {
            any_color = true;
            match color {
                Color::White => produced[0] = true,
                Color::Blue  => produced[1] = true,
                Color::Black => produced[2] = true,
                Color::Red   => produced[3] = true,
                Color::Green => produced[4] = true,
            }
        }
    }
    if !any_color {
        // Source produces only colorless mana.
        p.c += count;
    } else {
        let [w, u, b, r, g] = produced.map(|x| if x { count } else { 0 });
        p.w += w; p.u += u; p.b += b; p.r += r; p.g += g;
    }
}

// ── Simulation types ──────────────────────────────────────────────────────────


pub struct PlayerState {
    id: ObjId,
    deck_name: String,
    pub life: i32,
    /// Temporary protection from everything (The One Ring); cleared at this
    /// player's next untap step.
    pub(crate) protection_from_everything: bool,
    /// Number of lands played this turn; reset to 0 each Untap step. Engine enforces the one-per-turn rule.
    pub lands_played_this_turn: u8,
    /// Mana produced but not yet spent; drains at end of each step/phase.
    pub pool: ManaPool,
    /// Legacy outside-the-game storage retained for compatibility. Forge scenarios
    /// materialize sideboard cards as owned face-up exile objects so Karn can use
    /// one unified pool for wishboard cards and artifacts exiled during play.
    pub(crate) sideboard: Vec<String>,
    /// Number of cards drawn this turn; reset each Untap. Used for Bowmasters / Tamiyo triggers.
    draws_this_turn: u8,
    /// An unreplaced attempt to draw from an empty library, checked at the next SBA.
    failed_draw: bool,
    /// Ordered library: front = top of deck. Draw pops from front, shuffle randomizes.
    pub library_order: std::collections::VecDeque<ObjId>,
    /// How many top cards the controller legitimately KNOWS (front-down), since the
    /// last shuffle: set by reveal/tutor-to-top/scry/ordering, decremented on draw,
    /// reset to 0 on shuffle. 0 in a fresh (shuffled) opening hand — so reading the
    /// known top is not hidden information. Conservative: never claims more than known.
    pub known_top_len: usize,
    /// This player's decision policy (composed `Player { state, strategy }`). `None`
    /// until installed at sim init. The engine reaches it only via
    /// `SimState::with_strategy` (which falls back to `AlwaysPass`), so a
    /// player's agency is never shortcut in engine code.
    strategy: Option<Box<dyn Strategy>>,
}

impl PlayerState {
    pub fn new(deck: &str) -> Self {
        PlayerState {
            id: ObjId::UNSET,
            life: 20,
            protection_from_everything: false,
            deck_name: deck.to_string(),
            lands_played_this_turn: 0,
            pool: ManaPool::default(),
            sideboard: Vec::new(),
            draws_this_turn: 0,
            failed_draw: false,
            library_order: std::collections::VecDeque::new(),
            known_top_len: 0,
            strategy: None,
        }
    }

}

impl PlayerState {
    /// Clone game-visible player state for a search branch, deliberately dropping
    /// the strategy trait object. Search callers install fresh continuation
    /// strategies on the fork before simulating candidate actions.
    fn clone_for_search(&self) -> Self {
        Self {
            id: self.id,
            deck_name: self.deck_name.clone(),
            life: self.life,
            protection_from_everything: self.protection_from_everything,
            lands_played_this_turn: self.lands_played_this_turn,
            pool: self.pool.clone(),
            sideboard: self.sideboard.clone(),
            draws_this_turn: self.draws_this_turn,
            failed_draw: self.failed_draw,
            library_order: self.library_order.clone(),
            known_top_len: self.known_top_len,
            strategy: None,
        }
    }
}

pub struct SimState {
    /// The turn number currently being simulated. Set at the start of each do_turn call.
    pub current_turn: u8,
    /// Whether `Us` is on the play (vs. on the draw). Set once at game start.
    pub on_play: bool,
    /// The two players are `GameObject`s in `objects` with `ObjectRole::Player`;
    /// these are their stable ids. Reach their state via `player(who)`/`player_mut(who)`.
    pub us_id: ObjId,
    opp_id: ObjId,
    pub log: Vec<String>,
    /// Strategy decision log — records *why* the strategy made each choice.
    /// Populated by draining strategy structs' internal buffers after each call.
    pub decision_log: Vec<String>,
    /// Actions offered as legal that failed during execution; invalidates matchup outcomes.
    pub invalid_actions: u64,
    /// Set when the game ends by normal rules (a player's life reaches 0, etc.). Holds the winner.
    pub winner: Option<PlayerId>,
    /// Set when the active `Objective` decides the run has ended (e.g. Doomsday
    /// resolved). Replaces the former `success` flag / `Action::EndSimulation` sentinel.
    pub terminal: bool,
    /// Snapshot of `us`'s post-mulligan opening hand (catalog keys), for sample-game
    /// reporting. Populated by `run_game` once mulligans + the London bottom are done.
    pub opening_hand_us: Vec<String>,
    /// Hands `us` mulliganed away (each a snapshot of the 7 seen, in order), for
    /// sample-game reporting. Populated by `run_game` during the mulligan loop.
    pub mulliganed_hands_us: Vec<Vec<String>>,
    /// App-supplied objective: observes the event stream and decides termination.
    /// `None` for bare test states with no objective installed.
    pub(crate) objective: Option<Box<dyn crate::objective::Objective>>,
    /// Life total before Doomsday halved it (for display as "X → Y").
    pub life_before_dd: Option<i32>,
    /// Card being cast during the mana sub-loop (CR 601.2g). Set before mana loop,
    /// cleared after cast_spell. Used by Cavern of Souls to restrict colored mana.
    pub(crate) casting_spell: Option<ObjId>,
    /// Active player this phase/step (for log context).
    current_ap: ObjId,
    /// Current phase/step label (for log context).
    pub current_phase: Option<TurnPosition>,
    /// Attackers declared this combat (stable ObjIds); cleared at EndCombat.
    pub combat_attackers: Vec<ObjId>,
    /// Blocker assignments this combat: (attacker_id, blocker_id); cleared at EndCombat.
    combat_blocks: Vec<(ObjId, ObjId)>,
    /// Triggered abilities waiting to be pushed onto the stack at the next priority window.
    pending_triggers: Vec<TriggerContext>,
    /// Costs paid to cast the spell currently resolving (set by `resolve_top_of_stack`,
    /// cleared after resolution). Read by ETB replacement effects that need cast context.
    pub(crate) resolving_costs_ctx: CostsPaidCtx,
    /// Spell/ability stack. Items are resolved last-in-first-out. Populated by
    /// handle_priority_round; empty between priority rounds.
    pub stack: Vec<ObjId>,
    /// All objects in all zones, keyed by stable ObjId — cards (in any zone) AND
    /// card-less stack objects (abilities; see `GameObject.ability`).
    // Stable iteration order is shared by live games and search forks.
    pub objects: BTreeMap<ObjId, GameObject>,
    /// ID allocator — starts at 1; 0 is reserved as ObjId::UNSET.
    next_id: u64,
    /// Order in which cards entered each player's graveyard (oldest first). Used for display.
    graveyard_order: Vec<ObjId>,
    /// Cards exiled by a specific source with a linked return instruction (e.g. Portable Hole).
    /// Entries are removed when that source leaves the battlefield; cards still in exile
    /// are then returned to the battlefield.
    linked_exile: HashMap<ObjId, Vec<ObjId>>,
    /// All trigger instances for card objects in the simulation (pre-registered at init).
    /// `active` is false until the card enters the battlefield.
    pub(crate) trigger_instances: Vec<TriggerInstance>,
    /// All replacement instances for card objects in the simulation (pre-registered at init).
    /// `active` is false until the card enters the battlefield.
    pub(crate) replacement_instances: Vec<ReplacementInstance>,
    /// All prohibition instances (CR 614.17 "can't" effects). Checked before replacements.
    /// Replacements already applied in the current fire_event call chain (prevents loops).
    repl_applied: HashSet<(ObjId, usize)>,
    /// Recursion depth for fire_event (used to clear repl_applied at the top level).
    repl_depth: u32,
    /// All active continuous-effect instances. Checked at `recompute` time; expired entries
    /// are removed at Cleanup / start-of-turn as appropriate.
    pub(crate) continuous_instances: Vec<ContinuousInstance>,
    /// Emblems (CR 114): command-zone markers with static abilities. Their abilities
    /// are gathered fresh each `recompute` (like battlefield static abilities) and they
    /// never leave — emblems are not pruned. Created by `Action::CreateEmblem`.
    pub(crate) emblems: Vec<EmblemInstance>,
    /// Append-only record of every `GameEvent` that fired (post-prohibition, post-replacement).
    /// Layer B state surface for IR queries (`Expr::EventCount`, etc.) and the eventual
    /// replacement for scattered `this_turn` counters. See `ir/event_log.rs`.
    pub(crate) event_log: crate::ir::event_log::EventLog,
    /// Latent spell mods (CR 611.2f): consumed during 601.2a when a qualifying spell is cast.
    /// Entries expire at EndOfTurn cleanup if not consumed.
    pub(crate) latent_spell_mods: Vec<LatentSpellMod>,
    /// Monotonic counter for assigning timestamps to CIs (CR 613.6).
    pub(crate) ci_timestamp_counter: u32,
    /// Owned card catalog — populated once at sim init, never mutated.
    /// All runtime card-definition reads go through `state.def_of(id)` (live objects)
    /// or `state.catalog` (bootstrap / non-battlefield lookups).
    pub catalog: HashMap<String, CardDef>,
    /// RNG source for all in-simulation randomness (random discard, fetch, etc.).
    /// Effects access this directly via `state.rng`. Strategy functions receive their
    /// own rng parameter so their randomness remains independently injectable for tests.
    pub(crate) rng: Box<dyn rand::RngCore + Send>,
    /// Universal card evaluator: scores a card's value for `who` given current game state.
    /// Used by generic effect primitives (put_back, scry, order) to make strategy-driven
    /// card selection decisions. Higher = more valuable to keep.
    /// Wired at game init from matchup-parameterized strategy logic.
    /// Default: 0.5 (flat, no preference). Override per-game in simulate_game.
    pub(crate) evaluate_card:
        std::sync::Arc<dyn Fn(PlayerId, ObjId, &SimState) -> f64 + Send + Sync>,
}

impl SimState {
    /// Return the post-CE materialized `CardDef` for the object with the given id, if any.
    /// Returns `None` for naked stack abilities (no catalog entry) or unknown ids.
    pub fn def_of(&self, id: ObjId) -> Option<&CardDef> {
        let obj = self.objects.get(&id)?;
        // An ability on the stack is a card-less object — it has no CardDef.
        if obj.ability().is_some() { return None; }
        obj.materialized.as_ref()
    }
}

impl SimState {
    /// Make an isolated branch suitable for bounded search/rollouts.
    ///
    /// The branch preserves all rules-relevant game state and stable ObjIds, but
    /// deliberately drops the application objective and both strategy trait
    /// objects. Callers must install fresh strategies before advancing the fork.
    /// Randomness from this point forward is driven by `seed`, which makes each
    /// rollout reproducible without requiring the engine RNG trait object to be
    /// clonable.
    pub fn fork_for_search(&self, seed: u64) -> Self {
        let objects = self.objects.iter()
            .map(|(&id, obj)| (id, obj.clone_for_search()))
            .collect();
        Self {
            current_turn: self.current_turn,
            on_play: self.on_play,
            us_id: self.us_id,
            opp_id: self.opp_id,
            log: self.log.clone(),
            decision_log: self.decision_log.clone(),
            invalid_actions: self.invalid_actions,
            winner: self.winner,
            terminal: self.terminal,
            opening_hand_us: self.opening_hand_us.clone(),
            mulliganed_hands_us: self.mulliganed_hands_us.clone(),
            objective: None,
            life_before_dd: self.life_before_dd,
            casting_spell: self.casting_spell,
            current_ap: self.current_ap,
            current_phase: self.current_phase,
            combat_attackers: self.combat_attackers.clone(),
            combat_blocks: self.combat_blocks.clone(),
            pending_triggers: self.pending_triggers.clone(),
            resolving_costs_ctx: self.resolving_costs_ctx.clone(),
            stack: self.stack.clone(),
            objects,
            next_id: self.next_id,
            graveyard_order: self.graveyard_order.clone(),
            linked_exile: self.linked_exile.clone(),
            trigger_instances: self.trigger_instances.clone(),
            replacement_instances: self.replacement_instances.clone(),
            repl_applied: self.repl_applied.clone(),
            repl_depth: self.repl_depth,
            continuous_instances: self.continuous_instances.clone(),
            emblems: self.emblems.clone(),
            event_log: self.event_log.clone(),
            latent_spell_mods: self.latent_spell_mods.clone(),
            ci_timestamp_counter: self.ci_timestamp_counter,
            catalog: self.catalog.clone(),
            rng: Box::new(rand::rngs::StdRng::seed_from_u64(seed)),
            evaluate_card: Arc::clone(&self.evaluate_card),
        }
    }
}

impl SimState {
    pub fn new(us: PlayerState, opp: PlayerState) -> Self {
        let mut s = SimState {
            current_turn: 0,
            on_play: true,
            us_id: ObjId::UNSET,
            opp_id: ObjId::UNSET,
            log: Vec::new(),
            decision_log: Vec::new(),
            invalid_actions: 0,
            winner: None,
            terminal: false,
            opening_hand_us: Vec::new(),
            mulliganed_hands_us: Vec::new(),
            objective: None,
            life_before_dd: None,
            casting_spell: None,
            current_ap: ObjId::UNSET,
            current_phase: None,
            combat_attackers: Vec::new(),
            combat_blocks: Vec::new(),
            pending_triggers: Vec::new(),
            resolving_costs_ctx: CostsPaidCtx::default(),
            stack: Vec::new(),
            objects: BTreeMap::new(),
            next_id: 0,
            graveyard_order: Vec::new(),
            linked_exile: HashMap::new(),
            trigger_instances: Vec::new(),
            replacement_instances: Vec::new(),
            emblems: Vec::new(),
            repl_applied: HashSet::new(),
            repl_depth: 0,
            continuous_instances: Vec::new(),
            latent_spell_mods: Vec::new(),
            ci_timestamp_counter: 0,
            event_log: crate::ir::event_log::EventLog::new(),
            catalog: HashMap::new(),
            rng: Box::new(rand::rngs::StdRng::from_entropy()),
            evaluate_card: std::sync::Arc::new(|_, _, _| 0.5),
        };
        // Register the two players as GameObjects in the map (role = Player).
        let us_id = s.alloc_id();
        let opp_id = s.alloc_id();
        s.us_id = us_id;
        s.opp_id = opp_id;
        let mut us = us; us.id = us_id;
        let mut opp = opp; opp.id = opp_id;
        s.objects.insert(us_id, GameObject {
            id: us_id, catalog_key: String::new(),
            owner: PlayerId::Us, controller: PlayerId::Us, is_token: false,
            role: ObjectRole::Player(us), materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        s.objects.insert(opp_id, GameObject {
            id: opp_id, catalog_key: String::new(),
            owner: PlayerId::Opp, controller: PlayerId::Opp, is_token: false,
            role: ObjectRole::Player(opp), materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        s
    }

    /// Public scenario-construction helper: create a card object named `name`
    /// for `who` placed in `zone`, materializing its definition from `self.catalog`
    /// (which must already contain `name`). Returns the new object's id. This is
    /// the public way for apps/tests to build an arbitrary game state without
    /// reaching into `GameObject` internals. The stack is not supported here —
    /// use the cast/ability pipeline for that.
    pub fn place_card(&mut self, who: PlayerId, name: &str, zone: Zone) -> ObjId {
        assert!(!matches!(zone, Zone::Stack), "place_card: use the cast pipeline for the stack");
        let id = self.alloc_id();
        let mut obj = GameObject::new(id, name, who);
        obj.set_zone(zone);
        // Engine convention: only battlefield permanents carry a materialized def;
        // cards in other zones resolve their def from `catalog` (see `def_of`).
        if matches!(zone, Zone::Battlefield) {
            obj.materialized = self.catalog.get(name).cloned();
        }
        let ts = self.next_ci_timestamp();
        obj.ci_timestamp = ts;
        self.objects.insert(id, obj);
        if matches!(zone, Zone::Library) {
            self.player_mut(who).library_order.push_back(id);
        }
        id
    }

    pub(crate) fn next_ci_timestamp(&mut self) -> u32 {
        let t = self.ci_timestamp_counter;
        self.ci_timestamp_counter += 1;
        t
    }

    fn alloc_id(&mut self) -> ObjId {
        self.next_id += 1;
        ObjId(self.next_id)
    }

    pub fn permanents_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        self.objects.values().filter(move |c| c.controller == who && c.in_zone(Zone::Battlefield))
    }

    pub fn permanent_bf(&self, id: ObjId) -> Option<&BattlefieldState> {
        self.objects.get(&id)
            .filter(|c| c.in_zone(Zone::Battlefield))
            .and_then(|c| c.bf())
    }

    pub fn permanent_bf_mut(&mut self, id: ObjId) -> Option<&mut BattlefieldState> {
        self.objects.get_mut(&id)
            .filter(|c| c.in_zone(Zone::Battlefield))
            .and_then(|c| c.bf_mut())
    }

    pub fn hand_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        self.objects.values().filter(move |c| c.owner == who && c.in_zone(Zone::Hand { known: false }))
    }

    /// Cards in `who`'s hand whose identities are publicly known to the opponent.
    /// This is the safe accessor for external-agent observations: callers can
    /// expose the opponent hand size plus this iterator without leaking hidden
    /// cards from `hand_of(who)`.
    pub fn known_hand_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        self.objects.values().filter(move |c| {
            c.owner == who && matches!(&c.role, ObjectRole::Hand { known: true })
        })
    }

    pub fn graveyard_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        self.objects.values().filter(move |c| c.owner == who && c.in_zone(Zone::Graveyard))
    }

    pub fn exile_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        self.objects.values().filter(move |c| c.owner == who && c.in_zone(Zone::Exile { on_adventure: false }))
    }

    /// Legacy outside-the-game names. Forge scenarios currently leave this empty
    /// because sideboard cards are materialized in face-up exile at setup.
    pub fn sideboard_of(&self, who: PlayerId) -> &[String] {
        &self.player(who).sideboard
    }

    /// Remove one outside-the-game card by sideboard index. The caller may then
    /// materialize it into an in-game zone (e.g. Karn, the Great Creator -2).
    pub(crate) fn take_sideboard_card(&mut self, who: PlayerId, index: usize) -> Option<String> {
        if index < self.player(who).sideboard.len() {
            Some(self.player_mut(who).sideboard.remove(index))
        } else {
            None
        }
    }

    /// Cards owned by `who` that are currently in exile with adventure status.
    pub fn on_adventure_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        self.objects.values().filter(move |c| c.owner == who && c.zone() == Some(Zone::Exile { on_adventure: true }))
    }

    /// Iterate library in deck order (front = top). Yields `&GameObject` for each ObjId.
    pub fn library_of(&self, who: PlayerId) -> impl Iterator<Item = &GameObject> {
        let order = &self.player(who).library_order;
        order.iter().filter_map(move |id| self.objects.get(id))
    }

    pub fn hand_size(&self, who: PlayerId) -> i32 {
        self.hand_of(who).count() as i32
    }

    pub fn library_size(&self, who: PlayerId) -> usize {
        self.player(who).library_order.len()
    }

    /// True if `who` controls a source granting "no maximum hand size" (CR 402.2 /
    /// the Tamiyo −7 emblem). Derived on demand from emblems carrying a
    /// `CEMod::NoMaxHandSize` static, rather than a sticky player flag.
    pub(crate) fn has_no_max_hand_size(&self, who: PlayerId) -> bool {
        self.emblems.iter().any(|e| {
            e.controller == who
                && e.abilities.iter().any(|a| {
                    matches!(&a.kind,
                        crate::ir::ability::AbilityKind::Static { mods, .. }
                            if mods.iter().any(|m| matches!(m, crate::ir::ce::CEMod::NoMaxHandSize)))
                })
        })
    }

    /// Mutate zone field only — no triggers, no logging. Use `change_zone` for that.
    /// Maintains `library_order` when entering or leaving Library zone.
    fn set_card_zone(&mut self, id: ObjId, zone: Zone) {
        let (old_zone, owner) = match self.objects.get(&id) {
            Some(c) => (c.zone(), c.owner),
            None => return,
        };
        if old_zone == Some(Zone::Library) && zone != Zone::Library {
            self.player_mut(owner).library_order.retain(|&x| x != id);
        }
        if zone == Zone::Library && old_zone != Some(Zone::Library) {
            self.player_mut(owner).library_order.push_back(id);
        }
        if zone == Zone::Graveyard && old_zone != Some(Zone::Graveyard) {
            self.graveyard_order.push(id);
        } else if zone != Zone::Graveyard && old_zone == Some(Zone::Graveyard) {
            self.graveyard_order.retain(|&x| x != id);
        }
        if let Some(card) = self.objects.get_mut(&id) {
            card.set_zone(zone);
        }
    }



    /// Shuffle a player's library using the simulation RNG.
    fn shuffle_library(&mut self, who: PlayerId) {
        use rand::seq::SliceRandom;
        // Disjoint borrows: the player object (in `objects`) and `rng` are
        // separate fields, so reach the library via direct `objects` access
        // (not the `player_mut` method, which would borrow all of `self`).
        let id = self.player_id(who);
        if let Some(ps) = self.objects.get_mut(&id).and_then(|o| o.player_state_mut()) {
            ps.library_order.make_contiguous().shuffle(&mut *self.rng);
            ps.known_top_len = 0; // a shuffle scrambles the known prefix
        }
    }

    /// True when the simulation should stop (game ended or objective reached).
    fn done(&self) -> bool {
        self.winner.is_some() || self.terminal
    }

    pub fn player(&self, who: PlayerId) -> &PlayerState {
        let id = self.player_id(who);
        self.objects.get(&id).and_then(|o| o.player_state())
            .expect("player object present with Player role")
    }

    pub fn player_mut(&mut self, who: PlayerId) -> &mut PlayerState {
        let id = self.player_id(who);
        self.objects.get_mut(&id).and_then(|o| o.player_state_mut())
            .expect("player object present with Player role")
    }

    /// Run `f` with player `p`'s `Strategy` and an immutable view of the whole
    /// state. The strategy is moved out for the duration (breaking the
    /// self-borrow: a strategy lives *on* the state it must observe), then put
    /// back. This is the single channel through which engine code reaches a
    /// player's decisions — resolution included — so player agency is never
    /// shortcut inline. A missing strategy panics in production (every player must
    /// have one); under `cfg(test)` it falls back to `AlwaysPass` for bare states.
    /// Install player `p`'s decision policy (the composed Player { state, strategy }).
    pub(crate) fn set_strategy(&mut self, p: PlayerId, s: Box<dyn crate::strategy::Strategy>) {
        self.player_mut(p).strategy = Some(s);
    }

    pub(crate) fn with_strategy<R>(
        &mut self,
        p: PlayerId,
        f: impl FnOnce(&mut dyn crate::strategy::Strategy, &mut SimState) -> R,
    ) -> R {
        // Move the strategy out so it and the rest of `self` are disjoint — `f`
        // gets both `&mut Strategy` and `&mut SimState` (needed by the cast/
        // activate submachines), with no self-borrow. Restored afterward.
        match self.player_mut(p).strategy.take() {
            Some(mut s) => {
                let r = f(&mut *s, self);
                self.player_mut(p).strategy = Some(s);
                r
            }
            // No strategy installed (or a re-entrant same-player call, which the engine
            // never makes). In production every player has a strategy (run_game installs
            // both), so this is a bug — don't silently no-op. Tests resolve bare states
            // via the AlwaysPass fallback.
            None => {
                #[cfg(test)]
                { let mut s = crate::strategy::AlwaysPass::new(p); f(&mut s, self) }
                #[cfg(not(test))]
                { panic!("with_strategy({p:?}): no strategy installed — the engine never \
                          substitutes a silent no-op in production"); }
            }
        }
    }

    /// Resolve a PlayerId to its stable ObjId.
    pub fn player_id(&self, who: PlayerId) -> ObjId {
        match who { PlayerId::Us => self.us_id, PlayerId::Opp => self.opp_id }
    }

    /// Active player for the current turn, if a turn is currently installed.
    /// Search/policy crates use this to avoid spending compute on priority
    /// windows where their player cannot take sorcery-speed strategic actions.
    pub fn active_player(&self) -> Option<PlayerId> {
        if self.current_ap == self.us_id {
            Some(PlayerId::Us)
        } else if self.current_ap == self.opp_id {
            Some(PlayerId::Opp)
        } else {
            None
        }
    }

    /// Resolve a player ObjId back to a PlayerId.
    fn who_pid(&self, id: ObjId) -> PlayerId {
        if id == self.us_id { PlayerId::Us } else { PlayerId::Opp }
    }

    /// True iff `id` is one of the player objects.
    fn is_player(&self, id: ObjId) -> bool {
        id == self.us_id || id == self.opp_id
    }

    /// Resolve a player ObjId back to the display string ("us"/"opp"). For logging only.
    fn who_str(&self, id: ObjId) -> &'static str {
        if id == self.us_id { "us" } else { "opp" }
    }

    /// Display name for a card, using the back-face name when the card is flipped.
    fn display_name(&self, card: &GameObject) -> String {
        if card.bf().map_or(false, |bf| bf.active_face == 1) {
            if let Some(back_name) = self.catalog.get(&card.catalog_key)
                .and_then(|d| d.back.as_ref())
                .map(|b| &b.name)
            {
                return back_name.clone();
            }
        }
        card.catalog_key.clone()
    }

    /// Return the name of the permanent with the given id.
    fn permanent_name(&self, id: ObjId) -> Option<String> {
        self.objects.get(&id)
            .filter(|c| c.in_zone(Zone::Battlefield))
            .map(|c| self.display_name(c))
    }

    /// Mana accessible right now for `who`: pool + what untapped permanents can still produce.
    ///
    /// Paid mana abilities are treated as filters rather than free mana.  In
    /// particular, Giant's Boulder ({1}, {T}: add one mana of any color) must
    /// never make the predictor believe it creates an extra mana from nothing.
    /// We first accumulate zero-mana-cost sources, then expose the color
    /// capabilities of payable filters without increasing total mana.
    pub fn potential_mana(&self, who: PlayerId) -> ManaPool {
        let mut p = self.player(who).pool.clone();
        let mut paid_filters: Vec<(Vec<Color>, usize, ManaCost)> = Vec::new();

        for card in self.permanents_of(who) {
            if let Some(bf) = card.bf() {
                let card_id = card.id;
                let mas = self.def_of(card_id).map(|d| d.mana_abilities()).unwrap_or(&[]);
                let avail: Vec<_> = mas.iter()
                    .filter(|ma| matches!(ma.source_zone, SourceZone::Battlefield))
                    .filter(|ma| ma.timing == ActivationTiming::Default)
                    .filter(|ma| !ma_requires_tap(ma) || !bf.tapped)
                    .filter(|ma| ma.condition.as_ref().map_or(true, |cond| obj_matches(cond, card_id, self)))
                    .cloned()
                    .collect();

                let free: Vec<_> = avail.iter()
                    .filter(|ma| ma.costs.first_mana_cost().map_or(true, |mc| mc.mana_value() == 0))
                    .cloned()
                    .collect();
                accumulate_source_potential(&free, bf.tapped, &mut p);

                for ma in avail {
                    if let Some(mc) = ma.costs.first_mana_cost() {
                        if mc.mana_value() > 0 {
                            paid_filters.push((ma.produces.clone(), ma.produces_count, mc));
                        }
                    }
                }
            }
        }

        // Hand-zone zero-cost mana abilities (e.g. Simian Spirit Guide).
        for card in self.hand_of(who) {
            if self.casting_spell == Some(card.id) { continue; }
            let mas = self.catalog.get(&card.catalog_key)
                .map(|d| d.mana_abilities()).unwrap_or(&[]);
            let free: Vec<_> = mas.iter()
                .filter(|ma| matches!(ma.source_zone, SourceZone::Hand))
                .filter(|ma| ma.costs.first_mana_cost().map_or(true, |mc| mc.mana_value() == 0))
                .cloned()
                .collect();
            accumulate_source_potential(&free, false, &mut p);
        }

        // A paid filter can change the color of mana already available, but it
        // does not increase the amount of mana available.  Add its color
        // capability only when the activation cost can be funded by the
        // free/current pool. `total` intentionally stays unchanged.
        for (colors, produces_count, activation_cost) in paid_filters {
            if !p.can_pay(&activation_cost) {
                continue;
            }
            let count = produces_count as i32;
            for color in colors {
                match color {
                    Color::White => p.w = p.w.max(count),
                    Color::Blue  => p.u = p.u.max(count),
                    Color::Black => p.b = p.b.max(count),
                    Color::Red   => p.r = p.r.max(count),
                    Color::Green => p.g = p.g.max(count),
                }
            }
        }

        p
    }

    /// Count of NONCREATURE spells `who` has cast this turn, read from the event log (the
    /// engine's per-turn spell tally — the replacement for a scattered counter field). Used
    /// by goldfish line-planning: The Fantasticar pops on the controller's fourth noncreature
    /// spell, so the strategy must know how many have been cast to avoid firing the car as
    /// that fourth spell (it would be on the stack, not in play, and the trigger would whiff).
    pub fn noncreature_casts_this_turn(&self, who: PlayerId) -> u32 {
        self.event_log.count(crate::ir::event_log::Window::ThisTurn, |e| {
            matches!(e.event, GameEvent::SpellCast { caster, card_id, .. }
                if caster == who
                    && self.objects.get(&card_id)
                        .and_then(|o| self.catalog.get(&o.catalog_key))
                        .map_or(true, |d| !d.is_creature()))
        }) as u32
    }

    fn life_of(&self, who: PlayerId) -> i32 {
        self.player(who).life
    }

    fn lose_life(&mut self, who: PlayerId, n: i32) {
        self.player_mut(who).life -= n;
        // Log actual life loss (CR 118.2) so event-log queries can derive
        // "lost life this turn" without a bespoke per-player counter.
        if n > 0 {
            let turn = self.current_turn as u32;
            self.event_log.push(turn, GameEvent::LifeLost { who, amount: n });
        }
    }

    fn gain_life(&mut self, who: PlayerId, n: i32) {
        self.player_mut(who).life += n;
    }

    fn log(&mut self, t: u8, who: PlayerId, msg: impl Into<String>) {
        let phase_str = match self.current_phase {
            Some(TurnPosition::Step(s))  => format!("{:?}", s),
            Some(TurnPosition::Phase(p)) => format!("{:?}", p),
            None                         => String::new(),
        };
        let ctx = if self.current_ap != ObjId::UNSET {
            format!("|{}/{}", self.who_str(self.current_ap), phase_str)
        } else {
            String::new()
        };
        self.log.push(format!("T{} [{}{}] {}", t, who, ctx, msg.into()));
    }


    pub fn stack_item_owner(&self, id: ObjId) -> ObjId {
        if let Some(card) = self.objects.get(&id) {
            return self.player_id(card.owner);
        }
        ObjId::UNSET
    }

    pub fn stack_item_display_name(&self, id: ObjId) -> &str {
        if let Some(card) = self.objects.get(&id) {
            return card.catalog_key.as_str();
        }
        ""
    }

    /// True iff `id` is a stack item (spell or ability) that a counter could target.
    /// Every object with zone==Stack — spell or ability — is a legal target;
    /// "can't be countered" is enforced at resolution, not targeting (CR 608.2b).
    pub fn stack_item_is_counterable(&self, id: ObjId) -> bool {
        self.objects.get(&id).map_or(false, |o| o.in_zone(Zone::Stack))
    }

    /// Place an ability (a card-less stack object) on the stack. `id` must be freshly
    /// allocated; `source_name` becomes its display name (`catalog_key`). The object's
    /// `ability` payload marks it card-less (CR 113.7 / 608).
    pub(crate) fn insert_stack_ability(
        &mut self,
        id: ObjId,
        source_name: impl Into<String>,
        controller: PlayerId,
        ability: AbilityState,
    ) {
        let mut obj = GameObject::new(id, source_name, controller);
        obj.role = ObjectRole::StackAbility(ability);
        self.objects.insert(id, obj);
        self.stack.push(id);
    }
}


// ── Display ───────────────────────────────────────────────────────────────────

fn stage_label(turn: u8) -> &'static str {
    match turn {
        0..=3 => "Early",
        4..=5 => "Mid",
        _ => "Late",
    }
}

fn sec(label: &str) -> String {
    let total = 50usize;
    let label_with_spaces = format!(" {} ", label);
    let padding = total.saturating_sub(label_with_spaces.chars().count() + 2);
    format!("  ──{}{}", label_with_spaces, "─".repeat(padding))
}

// PlayerState Display is handled via SimState::fmt_player_zones which has access to state.objects.

impl SimState {
    /// Write hand/graveyard/exile zones for `who` to the formatter — one line per zone.
    fn fmt_player_zones(&self, f: &mut std::fmt::Formatter<'_>, who: PlayerId, reveal_hand: bool) -> std::fmt::Result {
        let mut visible: Vec<&str> = self.hand_of(who)
            .filter(|c| reveal_hand || matches!(c.zone(), Some(Zone::Hand { known: true })))
            .map(|c| c.catalog_key.as_str())
            .collect();
        visible.sort();
        let hidden = if reveal_hand { 0 } else {
            self.hand_of(who)
                .filter(|c| matches!(c.zone(), Some(Zone::Hand { known: false })))
                .count()
        };
        if visible.len() + hidden > 0 {
            let mut parts = Self::collapse_counts(visible.iter().map(|s| s.to_string()).collect());
            if hidden > 0 { parts.push(format!("({} hidden)", hidden)); }
            writeln!(f, "  Hand      : {}", parts.join(", "))?;
        }

        let gy: Vec<String> = self.graveyard_order.iter()
            .filter_map(|id| self.objects.get(id))
            .filter(|c| c.owner == who && c.in_zone(Zone::Graveyard))
            .map(|c| c.catalog_key.clone())
            .collect();
        if !gy.is_empty() {
            writeln!(f, "  Graveyard : {}", gy.join(", "))?;
        }

        let mut exile: Vec<String> = self.exile_of(who)
            .map(|c| if matches!(c.zone(), Some(Zone::Exile { on_adventure: true })) {
                format!("{} (adv)", c.catalog_key)
            } else {
                c.catalog_key.clone()
            })
            .collect();
        if !exile.is_empty() {
            exile.sort();
            writeln!(f, "  Exile     : {}", Self::collapse_counts(exile).join(", "))?;
        }

        Ok(())
    }

    /// Collapse a list of display strings into `"Name ×N"` entries, preserving first-seen order.
    fn collapse_counts(items: Vec<String>) -> Vec<String> {
        let mut seen: Vec<(String, usize)> = Vec::new();
        for item in items {
            if let Some(entry) = seen.iter_mut().find(|(s, _)| *s == item) {
                entry.1 += 1;
            } else {
                seen.push((item, 1));
            }
        }
        seen.into_iter().map(|(s, n)| if n > 1 { format!("{} ×{}", s, n) } else { s }).collect()
    }

    /// Write permanents for `who` — lands on one line, non-lands on another.
    fn fmt_permanents(&self, f: &mut std::fmt::Formatter<'_>, who: PlayerId) -> std::fmt::Result {
        let fmt_perm = |card: &&GameObject| -> Option<String> {
            let bf = card.bf()?;
            let mut tags: Vec<String> = Vec::new();
            if bf.counters != 0 { tags.push(format!("{:+}", bf.counters)); }
            if bf.loyalty > 0   { tags.push(format!("loy:{}", bf.loyalty)); }
            if bf.tapped         { tags.push("tapped".into()); }
            let suffix = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) };
            Some(format!("{}{}", self.display_name(card), suffix))
        };

        let mut lands: Vec<&GameObject> = self.permanents_of(who)
            .filter(|c| c.bf().is_some() && self.def_of(c.id).map_or(false, |d| d.is_land()))
            .collect();
        let tapped_first = |a: &&GameObject, b: &&GameObject| {
            let a_tap = a.bf().map_or(false, |bf| bf.tapped);
            let b_tap = b.bf().map_or(false, |bf| bf.tapped);
            b_tap.cmp(&a_tap).then(a.catalog_key.cmp(&b.catalog_key))
        };
        lands.sort_by(tapped_first);

        let mut others: Vec<&GameObject> = self.permanents_of(who)
            .filter(|c| c.bf().is_some() && !self.def_of(c.id).map_or(false, |d| d.is_land()))
            .collect();
        others.sort_by(tapped_first);

        if !lands.is_empty() {
            let items = Self::collapse_counts(lands.iter().filter_map(fmt_perm).collect());
            writeln!(f, "  Lands     : {}", items.join(", "))?;
        }
        if !others.is_empty() {
            let items = Self::collapse_counts(others.iter().filter_map(fmt_perm).collect());
            writeln!(f, "  Permanents: {}", items.join(", "))?;
        }
        Ok(())
    }
}

impl std::fmt::Display for SimState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let dbar = "═".repeat(50);
        writeln!(f)?;
        writeln!(f, "  ╔{}╗", dbar)?;
        writeln!(f, "  ║{:^50}║", " DOOMSDAY PILE SCENARIO ")?;
        writeln!(f, "  ╚{}╝", dbar)?;
        writeln!(f)?;
        writeln!(f, "  Deck    : {}", self.player(PlayerId::Us).deck_name)?;
        writeln!(f, "  Opponent: {}", self.player(PlayerId::Opp).deck_name)?;
        writeln!(
            f,
            "  Turn    : {} ({}, {})",
            self.current_turn,
            stage_label(self.current_turn),
            if self.on_play { "on the play" } else { "on the draw" }
        )?;

        if !self.decision_log.is_empty() {
            writeln!(f)?;
            writeln!(f, "{}", sec("STRATEGY DECISIONS"))?;
            writeln!(f)?;
            for entry in &self.decision_log {
                writeln!(f, "  {}", entry)?;
            }
        }

        if !self.log.is_empty() {
            writeln!(f)?;
            writeln!(f, "{}", sec("TURN LOG"))?;
            writeln!(f)?;
            for entry in &self.log {
                writeln!(f, "  {}", entry)?;
            }
        }

        writeln!(f)?;
        writeln!(f, "{}", sec("MY BOARD"))?;
        writeln!(f)?;
        writeln!(f, "  Life       : {}", self.player(PlayerId::Us).life)?;
        self.fmt_permanents(f, PlayerId::Us)?;
        self.fmt_player_zones(f, PlayerId::Us, true)?;
        writeln!(f)?;

        let opp_label = format!("OPPONENT: {}", self.player(PlayerId::Opp).deck_name);
        writeln!(f, "{}", sec(&opp_label))?;
        writeln!(f)?;
        writeln!(f, "  Life       : {}", self.player(PlayerId::Opp).life)?;
        self.fmt_permanents(f, PlayerId::Opp)?;
        self.fmt_player_zones(f, PlayerId::Opp, false)?;

        Ok(())
    }
}
// ── Structured output ────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ScenarioResult {
    pub turn: u8,
    pub stage: String,
    pub on_play: bool,
    pub us: PlayerResult,
    pub opp: PlayerResult,
    pub log: Vec<String>,
    /// Cards on the stack (e.g. Doomsday mid-resolution).
    pub stack: Vec<String>,
    /// Life before Doomsday halved it (for "X → Y" display).
    pub life_before_dd: Option<i32>,
    /// Strategy decision log — records reasoning behind each choice.
    pub decision_log: Vec<String>,
    /// Full text representation for sharing/debugging.
    pub text_summary: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct PlayerResult {
    pub deck_name: String,
    pub life: i32,
    pub lands: Vec<PermanentResult>,
    pub permanents: Vec<PermanentResult>,
    pub hand: Vec<CardResult>,
    pub hand_hidden: usize,
    pub land_drop_available: bool,
    pub library: Vec<String>,
    pub graveyard: Vec<String>,
    pub exile: Vec<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct PermanentResult {
    pub name: String,
    pub tapped: bool,
    pub counters: i32,
    pub loyalty: i32,
    pub flipped: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct CardResult {
    pub name: String,
}

impl SimState {
    pub fn to_result(&self) -> ScenarioResult {
        let stack = if self.terminal {
            vec!["Doomsday".to_string()]
        } else {
            vec![]
        };

        ScenarioResult {
            turn: self.current_turn,
            stage: stage_label(self.current_turn).to_string(),
            on_play: self.on_play,
            us: self.player_result(PlayerId::Us),
            opp: self.player_result(PlayerId::Opp),
            log: self.log.clone(),
            decision_log: self.decision_log.clone(),
            stack,
            life_before_dd: self.life_before_dd,
            text_summary: format!("{}", self),
        }
    }

    fn player_result(&self, who: PlayerId) -> PlayerResult {
        let is_land = |c: &&GameObject| -> bool {
            c.bf().is_some()
                && !self.def_of(c.id).map(|d| d.mana_abilities()).unwrap_or(&[]).is_empty()
        };

        let to_perm = |c: &GameObject| -> PermanentResult {
            let bf = c.bf().unwrap();
            PermanentResult {
                name: self.display_name(c),
                tapped: bf.tapped,
                counters: bf.counters,
                loyalty: bf.loyalty,
                flipped: bf.active_face == 1,
            }
        };

        let lands: Vec<PermanentResult> = self.permanents_of(who)
            .filter(is_land)
            .map(|c| to_perm(c))
            .collect();

        let permanents: Vec<PermanentResult> = self.permanents_of(who)
            .filter(|c| !is_land(c))
            .map(|c| to_perm(c))
            .collect();

        // Our hand is fully known to us; opponent's uses the known/hidden split.
        let (hand, hand_hidden) = if who == PlayerId::Us {
            let all: Vec<CardResult> = self.hand_of(who)
                .map(|c| CardResult { name: c.catalog_key.clone() })
                .collect();
            (all, 0)
        } else {
            let known: Vec<CardResult> = self.hand_of(who)
                .filter(|c| matches!(c.zone(), Some(Zone::Hand { known: true })))
                .map(|c| CardResult { name: c.catalog_key.clone() })
                .collect();
            let hidden = self.hand_of(who)
                .filter(|c| matches!(c.zone(), Some(Zone::Hand { known: false })))
                .count();
            (known, hidden)
        };

        // Only reveal our library (opponent's is hidden).
        let library: Vec<String> = if who == PlayerId::Us {
            let mut lib: Vec<String> = self.library_of(who)
                .map(|c| c.catalog_key.clone())
                .collect();
            lib.sort();
            lib
        } else {
            vec![]
        };

        let mut graveyard: Vec<String> = self.graveyard_order.iter()
            .filter_map(|id| self.objects.get(id))
            .filter(|c| c.owner == who && c.in_zone(Zone::Graveyard))
            .map(|c| c.catalog_key.clone())
            .collect();

        // If DD just resolved, it's in the GY but we display it on the stack instead.
        if who == PlayerId::Us && self.terminal {
            if let Some(pos) = graveyard.iter().rposition(|n| n == "Doomsday") {
                graveyard.remove(pos);
            }
        }

        let exile: Vec<String> = self.exile_of(who)
            .map(|c| if matches!(c.zone(), Some(Zone::Exile { on_adventure: true })) {
                format!("{} (adv)", c.catalog_key)
            } else {
                c.catalog_key.clone()
            })
            .collect();

        let land_drop_available = self.player(who).lands_played_this_turn == 0;

        PlayerResult {
            deck_name: self.player(who).deck_name.clone(),
            life: self.player(who).life,
            lands,
            permanents,
            hand,
            hand_hidden,
            land_drop_available,
            library,
            graveyard,
            exile,
        }
    }
}

// ── Turn simulation ───────────────────────────────────────────────────────────


/// Play a specific, pre-chosen land from hand (moves it to Battlefield).
/// Fetches stay in play to be cracked later in the ability pass.
fn sim_play_land(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    card_id: ObjId,
) {
    if state.player(who).lands_played_this_turn >= 1 { return; }
    let land_name = match state.objects.get(&card_id) {
        Some(c) if c.in_zone(Zone::Hand { known: false }) => c.catalog_key.clone(),
        _ => return,
    };
    state.log(t, who, format!("Play {} [hand: {}]", land_name, state.hand_size(who)));
    change_zone(card_id, ZoneId::Battlefield, state, t, who);
    fire_event(GameEvent::LandPlayed { id: card_id, controller: who }, state, t, who);
}


/// Discard down to 7 at end of turn.
fn sim_discard_to_limit(state: &mut SimState, t: u8, who: PlayerId) {
    if state.has_no_max_hand_size(who) { return; }
    let hand = state.hand_size(who);
    if hand > 7 {
        let n = hand - 7;
        // Discard n cards (move from Hand to Graveyard).
        let to_discard: Vec<ObjId> = state.hand_of(who).take(n as usize).map(|c| c.id).collect();
        for id in to_discard {
            state.set_card_zone(id, Zone::Graveyard);
        }
        state.log(t, who, format!("Discard {} to hand limit", n));
    }
}

// ── Action system ─────────────────────────────────────────────────────────────

// resolve_who, matches_target_type, has_valid_target, ability_available,
// collect_legal_actions, choose_land_name
// are defined in strategy.rs / predicates.rs

// choose_permanent_target is defined in predicates.rs

pub(crate) fn card_zone_to_id(zone: &Zone) -> ZoneId {
    match zone {
        Zone::Library        => ZoneId::Library,
        Zone::Hand { .. }    => ZoneId::Hand,
        Zone::Stack          => ZoneId::Stack,
        Zone::Battlefield    => ZoneId::Battlefield,
        Zone::Graveyard      => ZoneId::Graveyard,
        Zone::Exile { .. }   => ZoneId::Exile,
    }
}


/// Offset added to IR-replacement indices when forming `repl_applied` keys,
/// so they never collide with legacy `replacement_defs` indices on the same
/// object. Any value larger than a card's plausible replacement count works;
/// `usize::MAX / 2` leaves room on both sides.
pub(crate) const IR_REPL_KEY_BASE: usize = usize::MAX / 2;

/// The central elemental event pipeline.
///
/// Stage order per the Comprehensive Rules:
///   1. Prohibition check (CR 614.17 — "can't" effects; if any match, event is suppressed)
///   2. Replacement check (CR 614 — first applicable replacement fires instead)
///   3. Do effect (state mutation for this event type)
///   4. Log
///   5. Trigger dispatch (CR 603 — collect triggered abilities)
///   6. Recompute CE materialization (at top-level depth only)
/// Returns `true` iff the event was suppressed by a "can't" prohibition (CR 614.17).
pub(crate) fn fire_event(
    event: GameEvent,
    state: &mut SimState,
    t: u8,
    actor: PlayerId,
) -> bool {
    state.repl_depth += 1;
    if state.repl_depth == 1 {
        state.repl_applied.clear();
    }

    // Stage 1: Prohibition check (CR 614.17).
    // "Can't" effects are not replacements — they suppress the event outright and
    // take precedence over replacements. Walk all objects and check prohibition_defs
    // from catalog, filtered by active_when predicate.
    let prohibited = state.objects.iter().any(|(id, obj)| {
        state.catalog.get(&obj.catalog_key).map_or(false, |card_def| {
            // Closure prohibitions (legacy path, being retired).
            card_def.prohibition_defs.iter().any(|pdef| {
                (pdef.active_when)(*id, state) && (pdef.check)(&event, *id, obj.controller, state)
            })
            // IR prohibitions: `AbilityKind::Prohibition { matches, active_zone }` —
            // the event pipeline's single "can't" mechanism (CR 101.2). The pattern
            // matches the proposed event; a match suppresses it. `active_zone`, when
            // `Some`, gates the source by zone (Battlefield for static permanents like
            // Grafdigger's), so the prohibition stops functioning once its source
            // leaves that zone; `None` leaves a self-scoping pattern (`It == Source`)
            // to narrow relevance on its own.
            || card_def.abilities.iter().any(|ab| {
                if let crate::ir::ability::AbilityKind::Prohibition { matches, active_zone } = &ab.kind {
                    if let Some(z) = active_zone {
                        if !crate::ir::executor::obj_in_kind(obj, z.clone()) { return false; }
                    }
                    let env = crate::ir::executor::BindEnv::new()
                        .with_source(*id)
                        .with_controller(obj.controller);
                    crate::ir::executor::match_event_pattern(matches, &event, &env, state).is_some()
                } else {
                    false
                }
            })
        })
    });
    if prohibited {
        state.log(t, actor, "→ event prohibited (\"can't\" effect)".to_string());
        state.repl_depth -= 1;
        return true;
    }

    // Stage 2: Replacement check.
    // Part A: Card-bound replacements — walk all objects, check replacement_defs from catalog.
    // Part B: Ephemeral replacements — check replacement_instances (runtime-created, e.g. FoN exile).
    // First active, non-applied replacement that matches wins (CR 614.5 loop prevention).
    let repl_match = {
        let mut found = None;
        // Part A: card-bound replacements from catalog.
        for (id, obj) in &state.objects {
            let card_def = match state.catalog.get(&obj.catalog_key) {
                Some(d) => d,
                None => continue,
            };
            for (def_idx, rdef) in card_def.replacement_defs.iter().enumerate() {
                let key = (*id, def_idx);
                if !(rdef.active_when)(*id, state) { continue; }
                if state.repl_applied.contains(&key) { continue; }
                if let Some(targets) = (rdef.check)(&event, *id, obj.controller, state) {
                    let effect = (rdef.make_effect)(*id, obj.controller);
                    found = Some((key, targets, effect));
                    break;
                }
            }
            if found.is_some() { break; }
        }
        // Part A-IR: card-bound IR Replacement abilities from catalog.
        // Keys are offset by IR_REPL_KEY_BASE so they don't collide with
        // legacy `def_idx` keys.
        if found.is_none() {
            for (id, obj) in &state.objects {
                let card_def = match state.catalog.get(&obj.catalog_key) {
                    Some(d) => d,
                    None => continue,
                };
                for (ab_idx, ability) in card_def.abilities.iter().enumerate() {
                    let crate::ir::ability::AbilityKind::Replacement { active_zone, .. } = &ability.kind
                    else {
                        continue;
                    };
                    // Zone gate (mirrors the Prohibition walk): a static-permanent
                    // replacement (Leyline, Containment Priest) only functions while
                    // its source is on the battlefield; `None` (self-entry) is always
                    // consulted.
                    if let Some(z) = active_zone {
                        if !crate::ir::executor::obj_in_kind(obj, z.clone()) { continue; }
                    }
                    let key = (*id, IR_REPL_KEY_BASE + ab_idx);
                    if state.repl_applied.contains(&key) { continue; }
                    let Some(targets) = crate::ir::executor::ir_replacement_match(
                        ability, &event, *id, obj.controller, state,
                    ) else { continue };
                    let Some(effect) = crate::ir::executor::ir_replacement_effect(
                        ability, *id, obj.controller, &event,
                    ) else { continue };
                    found = Some((key, targets, effect));
                    break;
                }
                if found.is_some() { break; }
            }
        }
        // Part B: ephemeral replacement instances (runtime-created by abilities).
        if found.is_none() {
            for (idx, inst) in state.replacement_instances.iter().enumerate() {
                let key = (inst.source_id, idx);
                if state.repl_applied.contains(&key) { continue; }
                if let Some(targets) = (inst.check)(&event, inst.source_id, inst.controller, state) {
                    found = Some((key, targets, inst.effect.clone()));
                    break;
                }
            }
        }
        found
    };

    if let Some((repl_key, targets, effect)) = repl_match {
        state.repl_applied.insert(repl_key);
        effect.call(state, t, &targets);
        state.repl_depth -= 1;
        return false; // original effect suppressed by replacement (not a prohibition)
    }

    // An empty-library draw can still be replaced above. If it is not,
    // remember the failed attempt for the next SBA; no card was drawn, so do
    // not emit draw triggers or count it toward the successful-draw guard.
    if let GameEvent::Draw { controller, .. } = &event {
        if state.player(*controller).library_order.is_empty() {
            let ps = state.player_mut(*controller);
            ps.failed_draw = true;
            state.log(t, *controller, "Attempted to draw from an empty library");
            state.repl_depth -= 1;
            return false;
        }
    }

    // Stage 3: Apply state mutation.
    do_effect(&event, state);

    // Stage 3b: Record on the event log (Layer B). Pushed after do_effect so the
    // log reflects events that actually happened (post-prohibition, post-replacement).
    state.event_log.push(t as u32, event.clone());

    // Stage 4: Log.
    log_event(&event, state, t, actor);

    // Stage 4.5: Objective observation. The active objective decides termination
    // off the event stream (replaces the former EndSimulation/success sentinel).
    // Moved out and restored so it and the rest of `state` are disjoint, mirroring
    // the `with_strategy` pattern.
    if let Some(mut obj) = state.objective.take() {
        if obj.observe(&event, state) {
            state.terminal = true;
        }
        state.objective = Some(obj);
    }

    // Stage 5: Trigger dispatch.
    let (triggers, one_shot_fired) = fire_triggers(&event, state);
    state.pending_triggers.extend(triggers);
    // CR 714.2: as a Saga enters, its controller puts a lore counter on it,
    // firing chapter I. (Turn-based action, not a triggered ability — handled
    // here so the chapter joins the same pending-trigger batch as ETB triggers.)
    if let GameEvent::ZoneChange { id, to: ZoneId::Battlefield, .. } = &event {
        if obj_is_saga(state, *id) {
            add_lore_counter(state, *id, t);
        }
    }
    // Remove OneShot trigger instances that just fired (reverse order to keep indices valid).
    for &i in one_shot_fired.iter().rev() {
        state.trigger_instances.remove(i);
    }


    state.repl_depth -= 1;
    if state.repl_depth == 0 {
        // Rebuild the inlined materialized snapshot after every top-level tick so that
        // strategy, display, and combat damage always see a current, CE-adjusted view.
        recompute(state);
    }
    false
}

fn do_effect(event: &GameEvent, state: &mut SimState) {
    match event {
        GameEvent::ZoneChange { id, from, to, .. } => {
            let id = *id;
            let from = *from;
            let to = *to;

            let new_zone = match to {
                ZoneId::Graveyard   => Zone::Graveyard,
                ZoneId::Exile       => Zone::Exile { on_adventure: false },
                ZoneId::Hand        => Zone::Hand { known: false },
                ZoneId::Library     => Zone::Library,
                ZoneId::Stack       => Zone::Stack,
                ZoneId::Battlefield => Zone::Battlefield,
            };

            // Read owner and old zone before mutating, to maintain library_order.
            let (owner, old_zone) = match state.objects.get(&id) {
                Some(c) => (c.owner, c.zone()),
                None => return,
            };
            let zone_changed = old_zone != Some(new_zone);
            if zone_changed {
                if new_zone == Zone::Graveyard { state.graveyard_order.push(id); }
                else { state.graveyard_order.retain(|&x| x != id); }
                // Maintain library_order: remove from old library, add to new library.
                if old_zone == Some(Zone::Library) {
                    state.player_mut(owner).library_order.retain(|&x| x != id);
                }
                if new_zone == Zone::Library {
                    state.player_mut(owner).library_order.push_back(id);
                }
            }
            if zone_changed {
                if let Some(card) = state.objects.get_mut(&id) {
                    card.set_zone(new_zone);
                }
            }

            // Detach any equipment that was attached to the departing permanent (CR 301.5c).
            if from == ZoneId::Battlefield {
                for obj in state.objects.values_mut() {
                    if let Some(bf) = obj.bf_mut() {
                        if bf.attached_to == Some(id) {
                            bf.attached_to = None;
                        }
                    }
                }
            }

        }
        GameEvent::Draw { controller, .. } => {
            let controller = *controller;
            let ps = state.player_mut(controller);
            let top_id = ps.library_order.pop_front();
            ps.known_top_len = ps.known_top_len.saturating_sub(1); // top card left for hand
            if let Some(card_id) = top_id {
                ps.draws_this_turn = ps.draws_this_turn.saturating_add(1);
                state.set_card_zone(card_id, Zone::Hand { known: false });
            }
        }
        GameEvent::ManaProduced { who, ref spec } => {
            let mc = parse_mana_cost(spec);
            let pool = &mut state.player_mut(*who).pool;
            pool.w += mc.w; pool.u += mc.u; pool.b += mc.b;
            pool.r += mc.r; pool.g += mc.g; pool.c += mc.c;
            pool.total += mc.mana_value();
        }
        // EnteredStep, EnteredPhase, CreatureAttacked — notification events, no state mutation
        _ => {}
    }
}

fn log_event(event: &GameEvent, state: &mut SimState, t: u8, actor: PlayerId) {
    match event {
        GameEvent::ZoneChange { id, from, to, controller, .. } => {
            let card = state.objects.get(id).map(|o| o.catalog_key.as_str()).unwrap_or("?");
            match (from, to) {
                // Stack→Graveyard is silent here: resolution logs "{name} resolves" before calling
                // change_zone, and eff_counter_target logs "→ {name} countered" before setting zone
                // directly (bypassing change_zone). Logging here would produce a spurious "countered".
                (ZoneId::Stack,       ZoneId::Graveyard)   => {}
                (ZoneId::Battlefield, ZoneId::Graveyard)   => state.log(t, actor, format!("→ {} destroyed", card)),
                (ZoneId::Hand,        ZoneId::Graveyard)   => state.log(t, actor, format!("→ {} discarded", card)),
                (_,                   ZoneId::Graveyard)   => state.log(t, actor, format!("→ {} to graveyard", card)),
                (_,                   ZoneId::Exile)       => state.log(t, actor, format!("→ {} exiled", card)),
                (ZoneId::Hand,        ZoneId::Library)     => state.log(t, actor, format!("→ {} put back", card)),
                (_,                   ZoneId::Hand)        => state.log(t, actor, format!("→ {} returned to {}'s hand", card, controller)),
                (ZoneId::Graveyard,   ZoneId::Battlefield) => state.log(t, actor, format!("→ {} returns from graveyard", card)),
                _ => {}
            }
        }
        GameEvent::Draw { controller, draw_index, is_natural, card } => {
            let controller = *controller;
            let hand = state.hand_size(controller);
            // Name the drawn card so the play-by-play shows what was found (naturally or
            // off a cantrip); fall back to "a card" if the library was empty.
            let name = card.and_then(|id| state.objects.get(&id))
                .map(|o| o.catalog_key.as_str()).unwrap_or("a card");
            if *is_natural {
                state.log(t, controller, format!("Draw {} [hand: {}]", name, hand));
            } else {
                state.log(t, controller, format!("draw ({}) {} [hand: {}]", draw_index, name, hand));
            }
        }
        GameEvent::ManaProduced { who, ref spec } => {
            state.log(t, *who, format!("→ add {} to pool", spec));
        }
        _ => {}
    }
}

/// Move a game object from its current zone to `to`.
/// Works for any zone transition. No-ops silently if the id is not found.
/// Fires the event pipeline (replacements → state mutation → triggers → log).
pub(crate) fn change_zone(
    id: ObjId,
    to: ZoneId,
    state: &mut SimState,
    t: u8,
    actor: PlayerId,
) {
    let (_catalog_key, controller, from) = {
        let card = match state.objects.get(&id) {
            Some(c) => c,
            None => return,
        };
        (card.catalog_key.clone(), card.controller,
         card.zone().map(|z| card_zone_to_id(&z)).unwrap_or(ZoneId::Library))
    };
    // LTB: remove ephemeral CIs tied to this source.
    if from == ZoneId::Battlefield {
        state.continuous_instances.retain(|ci| {
            !(ci.source_id == id && ci.expiry == Expiry::WhileSourceOnBattlefield)
        });
    }
    // ETB: assign stable CI timestamp for static-ability CIs generated by recompute.
    if to == ZoneId::Battlefield {
        let ts = state.next_ci_timestamp();
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.ci_timestamp = ts;
        }
    }
    // Prohibitions are derived at fire time via active_when predicates.
    // Linked-exile effects (Portable Hole style) stop applying as soon as the
    // source leaves the battlefield. Capture the links before the source move,
    // then return any linked cards that are still exiled after the move resolves.
    let linked_to_return = if from == ZoneId::Battlefield && to != ZoneId::Battlefield {
        state.linked_exile.remove(&id).unwrap_or_default()
    } else {
        Vec::new()
    };

    fire_event(
        GameEvent::ZoneChange { id, actor, from, to, controller },
        state, t, actor,
    );

    for linked_id in linked_to_return {
        let still_exiled = state.objects.get(&linked_id)
            .map_or(false, |o| o.in_zone(Zone::Exile { on_adventure: false }));
        if still_exiled {
            if let Some(obj) = state.objects.get_mut(&linked_id) {
                obj.controller = obj.owner;
            }
            change_zone(linked_id, ZoneId::Battlefield, state, t, actor);
        }
    }
}

// matches_search_filter is defined in predicates.rs

/// Attempt a draw through the event pipeline. A completed draw increments
/// draws_this_turn; replacements and failed attempts do not count as draws.
fn sim_draw(state: &mut SimState, who: PlayerId, t: u8, is_natural: bool) {
    // Sanity guard: `draws_this_turn` is u8 — overflows silently at 256.
    // 200 draws in a single turn is almost certainly an unbounded-draw bug
    // (a Draw replacement / trigger that fires more draws than it consumes).
    // Panic with diagnostics so we get a reproducible crash site instead of
    // an obscure "attempt to add with overflow" later.
    let current = state.player(who).draws_this_turn;
    if current >= 200 {
        let tail: Vec<String> = state.decision_log.iter().rev().take(30).cloned().collect();
        let recent_log: Vec<String> = state.log.iter().rev().take(50).cloned().collect();
        panic!(
            "sim_draw: draws_this_turn for {:?} reached {} on turn {} (is_natural={}). \
             Likely unbounded-draw recursion.\n\
             Last 30 decision_log entries (newest first):\n{}\n\
             Last 50 main log entries (newest first):\n{}",
            who,
            current,
            t,
            is_natural,
            tail.iter().map(|s| format!("  {}", s)).collect::<Vec<_>>().join("\n"),
            recent_log.iter().map(|s| format!("  {}", s)).collect::<Vec<_>>().join("\n"),
        );
    }
    let draw_index = current + 1;
    // Peek the card about to be drawn (do_effect pops this same library top) so the log
    // can name it. Absent only on an empty library (no card is drawn then anyway).
    let card = state.player(who).library_order.front().copied();
    let ev = GameEvent::Draw { controller: who, draw_index, is_natural, card };
    fire_event(ev, state, t, who);
}


/// Pay ability costs via the IR cost driver and log successful activation.
/// Returns the objects moved during payment, or None if payment failed.
/// Hand-sourced abilities leave their source in hand until resolution.
fn pay_ability_cost(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    source_id: ObjId,
    ability: &AbilityDef,
    is_hand_source: bool,
) -> Option<CostsPaidCtx> {
    let source_name = state.permanent_name(source_id)
        .or_else(|| state.hand_of(who).find(|c| c.id == source_id).map(|c| c.catalog_key.clone()))
        .unwrap_or_default();
    let _ = is_hand_source; // ninja stays in hand until its ability resolves (CR 702.49d)

    // Pay cost via pay_ir_cost. The default-announcement (no strategy ref)
    // suffices for cost shapes whose strategy choice is a no-op (like
    // SacSelf with a single candidate). Mana availability is pre-checked
    // by `ability_available` and pre-filled by run_mana_loop above.
    let crate::ir::ability::CostBody::Ir(action) = &ability.costs;
    let ctx = pay_ir_cost(state, t, who, source_id, action, false)?;
    if ability.loyalty_delta().is_some() {
        if let Some(bf) = state.permanent_bf_mut(source_id) {
            bf.pw_activated_this_turn = true;
        }
    }
    state.log(t, who, format!("Activate {} ability", source_name));

    // Log loyalty adjustment.
    if let Some(n) = ability.loyalty_delta() {
        if let Some(new_loyalty) = state.permanent_bf(source_id).map(|bf| bf.loyalty) {
            state.log(t, who, format!("→ {} loyalty {} → {}", source_name,
                if n >= 0 { format!("+{}", n) } else { n.to_string() },
                new_loyalty));
        }
    }

    Some(ctx)
}


/// Cast a spell: pay its cost, choose any permanent target, remove from library, log,
/// and return the card's ObjId (now on the stack).
///
/// Cost selection: if `preferred_cost` is `Some`, that specific alternate cost is used
/// (caller already verified it's payable, e.g. `respond_with_counter` after prob checks).
/// Otherwise the standard mana cost is tried first; if unpayable (or mana_cost is empty
/// and the card has alternate costs), the first payable alternate cost is used instead.
///
/// Permanent targets (from `CardDef.target`) are chosen randomly at cast time and
/// locked into the SpellState on the card; resolution uses the stored target directly.
/// CR 611.2f: consume the first matching latent spell mod for this caster.
/// Called during 601.2a after the spell is on the stack. If a match is found,
/// the mod's factory produces a CI that is pushed to continuous_instances and
/// the LatentSpellMod is removed (consumed).
fn consume_latent_spell_mod(state: &mut SimState, caster: PlayerId, spell_id: ObjId) {
    let pos = state.latent_spell_mods.iter().position(|lsm| {
        lsm.controller == caster && (lsm.predicate)(spell_id, caster, state)
    });
    if let Some(i) = pos {
        let lsm = state.latent_spell_mods.remove(i);
        let ci = (lsm.make_ci)(spell_id, caster);
        state.continuous_instances.push(ci);
    }
}

/// Cast a spell identified by `card_id`, using the specified `face` (Main or Adventure).
/// Pays cost, builds effect, sets SpellState on the card object.
/// Returns `None` if the cast fails (cost unpayable, card missing).
fn cast_spell(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    card_id: ObjId,
    face: SpellFace,
    preferred_cost: Option<&AlternateCost>,
    announced_alt_index: Option<usize>,
    chosen_targets: &[ObjId],
    chosen_x: u32,
    chosen_mode: usize,
) -> Option<ObjId> {
    let name = state.objects.get(&card_id)?.catalog_key.clone();
    // Prefer the post-CE materialized def (current in normal game flow where recompute
    // runs before every priority window). Fall back to state.catalog for tests that call
    // cast_spell directly without a preceding recompute.
    let def = state.def_of(card_id)
        .cloned()
        .or_else(|| state.catalog.get(name.as_str()).cloned())?;

    if face == SpellFace::Back {
        let adv = def.adventure()?.clone();
        let is_sorcery = adv.is_sorcery();
        if is_sorcery && !state.stack.is_empty() {
            eprintln!("[priority] BUG: split-back sorcery {} on non-empty stack, treating as Pass", adv.name);
            return None;
        }
        // Casting legality is pre-checked via CardDef.castable (set by CEs in recompute).
        // Strategy only offers castable cards, so no prohibition gate needed here.
        let cost = parse_mana_cost(adv.mana_cost());
        state.player_mut(who).pool.spend(&cost);
        let (_adv_spec, adv_eff) = build_spell_effect(&adv, who, card_id, 0, 0);
        let adv_targets = chosen_targets.to_vec();
        let adv_target_label = if !chosen_targets.is_empty() {
            let names: Vec<String> = chosen_targets.iter()
                .map(|&tid| {
                    let name = state.stack_item_display_name(tid);
                    if name.is_empty() { format!("<?obj#{}>", tid.0) } else { name.to_string() }
                })
                .collect();
            format!(" targeting {}", names.join(", "))
        } else {
            String::new()
        };
        state.log(t, who, format!("Cast {} ({}, {}){} [hand: {}]", adv.name, adv.mana_cost(), name, adv_target_label, state.hand_size(who)));
        if let Some(card) = state.objects.get_mut(&card_id) {
            card.role = ObjectRole::StackSpell(SpellState {
                effect: Some(adv_eff),
                chosen_targets: adv_targets,
                is_back_face: true,
                costs_paid_ctx: CostsPaidCtx::default(),
            });
        }
        consume_latent_spell_mod(state, who, card_id);
        let back_mana_spent = mana_value(adv.mana_cost()) > 0;
        fire_event(GameEvent::SpellCast { caster: who, card_id, mana_spent: back_mana_spent, alt_cost: false, x: 0, delved: Vec::new() }, state, t, who);
        return Some(card_id);
    }

    // Main face: pay main cost (with delve and alternate costs).
    let mut cost = parse_mana_cost(def.mana_cost());

    // Delve: reduce generic cost by exiling cards from the caster's graveyard.
    let to_exile_ids: Vec<(ObjId, String)> = if def.delve() && cost.generic > 0 {
        let gy: Vec<(ObjId, String)> = state.graveyard_of(who).map(|c| (c.id, c.catalog_key.clone())).collect();
        let mut cards = Vec::new();
        for (id, card_name) in &gy {
            if cards.len() as i32 >= cost.generic { break; }
            cards.push((*id, card_name.clone()));
        }
        cost.generic -= cards.len() as i32;
        cards
    } else {
        Vec::new()
    };
    cost = apply_casting_cost_rules(cost, &def);

    // Empty mana_cost means the card has no castable mana cost (alt-cost-only, or truly uncostable).
    // Use mana_cost = "0" in the catalog for genuinely free spells (Lotus Petal, LED).
    let mana_is_usable = !def.mana_cost().is_empty() && state.potential_mana(who).can_pay(&cost);

    // Select cost. Track the index into def.alternate_costs() for evoke/similar triggers.
    let (alt_cost, alt_cost_idx): (Option<AlternateCost>, Option<usize>) = if let Some(pc) = preferred_cost {
        // Strategy announced an alt cost — use the announced index.
        (Some(pc.clone()), announced_alt_index)
    } else if !mana_is_usable {
        let found = def.alternate_costs()
            .iter()
            .enumerate()
            .find(|(_, c)| {
                if state.hand_size(who) < c.hand_min {
                    return false;
                }
                if c.condition.as_ref().map_or(false, |f| !f(who, state)) {
                    return false;
                }
                // Feasibility check: build_schema returns None when
                // candidates are missing or constants exceed limits.
                let crate::ir::ability::CostBody::Ir(action) = &c.costs;
                crate::ir::cost_exec::build_schema(action, state, who, card_id).is_some()
            });
        match found {
            Some((i, c)) => (Some(c.clone()), Some(i)),
            None => (None, None),
        }
    } else {
        (None, None)
    };

    if alt_cost.is_none() && !mana_is_usable {
        eprintln!("[cast-reject] {}: base cost {:?}, pool {:?}, potential {:?}",
            name, cost, state.player(who).pool, state.potential_mana(who));
        return None;
    }

    // XMana is an additional cost, but affordability must be checked together
    // with the selected base/alternate mana component.  Checking them
    // independently allowed the same three mana to satisfy both {C}{C} and X=3.
    let selected_base_mana = if let Some(ref alt) = alt_cost {
        let base = alt.costs.first_mana_cost().unwrap_or_default();
        apply_casting_cost_rules(base, &def)
    } else {
        cost.clone()
    };
    let combined_mana = combined_mana_cost_with_x(
        selected_base_mana,
        &def.additional_costs,
        chosen_x,
    );
    if !state.potential_mana(who).can_pay(&combined_mana) {
        eprintln!("[cast-reject] {}: combined cost {:?} (X={}), pool {:?}, potential {:?}",
            name, combined_mana, chosen_x, state.player(who).pool, state.potential_mana(who));
        return None;
    }
    if !can_pay_additional_ir_cost(state, who, card_id, &def.additional_costs, chosen_x) {
        eprintln!("[cast-reject] {}: additional cost schema (X={}), pool {:?}",
            name, chosen_x, state.player(who).pool);
        return None;
    }

    // Casting legality is pre-checked via CardDef.castable (set by CEs in recompute).
    // Strategy only offers castable cards, so no prohibition gate needed here.

    // Move to Stack zone (a placeholder spell payload; the real effect/targets/
    // costs are filled in once cost payment + `build_spell_effect` complete below).
    // Casting from the top of a library (Mystic Forge) must also remove the
    // object from the owner's ordered-library index; changing the object role
    // alone would otherwise leave a stale copy at the top.
    let was_in_library = state.objects.get(&card_id).map_or(false, |o| o.in_zone(Zone::Library));
    if was_in_library {
        state.player_mut(who).library_order.retain(|&id| id != card_id);
        let known = state.player(who).known_top_len;
        state.player_mut(who).known_top_len = known.saturating_sub(1);
    }
    if let Some(card) = state.objects.get_mut(&card_id) {
        card.role = ObjectRole::StackSpell(SpellState {
            effect: None,
            chosen_targets: Vec::new(),
            is_back_face: false,
            costs_paid_ctx: CostsPaidCtx::default(),
        });
    }

    // Pay cost and build a log label.
    let (cast_label, mut costs_ctx) = if let Some(ref alt) = alt_cost {
        // Trinisphere-style minimum applies to the mana component even when an
        // alternative cost is chosen. Pay any generic top-up separately.
        let alt_base = alt.costs.first_mana_cost().unwrap_or_default();
        let floored = apply_casting_cost_rules(alt_base.clone(), &def);
        let extra_generic = (floored.mana_value() - alt_base.mana_value()).max(0);
        if extra_generic > 0 {
            let extra = ManaCost { generic: extra_generic, ..Default::default() };
            if !state.player(who).pool.can_pay(&extra) { return None; }
            state.player_mut(who).pool.spend(&extra);
        }
        let crate::ir::ability::CostBody::Ir(action) = &alt.costs;
        let ctx = pay_ir_cost(state, t, who, card_id, action, true)?;
        ("ir alt cost".to_string(), ctx)
    } else {
        state.player_mut(who).pool.spend(&cost);
        (def.mana_cost().to_string(), CostsPaidCtx::default())
    };

    // Pay additional costs (CR 118.9d: apply regardless of which cost path was taken).
    // `chosen_x` is passed so XLife additional costs pay the strategy-chosen amount.
    if !def.additional_costs.is_empty() {
        // Preflight above should make this succeed.  If strategy choices or a
        // malformed cost still make payment fail, abort the cast rather than
        // resolving a spell whose additional cost was never actually paid.
        let add_ctx = pay_additional_ir_cost(
            state, t, who, card_id, &def.additional_costs, chosen_x, true
        )?;
        costs_ctx.objects_moved.extend(add_ctx.objects_moved);
        costs_ctx.returned_attack_targets.extend(add_ctx.returned_attack_targets);
        costs_ctx.chosen_x = add_ctx.chosen_x;
    }
    costs_ctx.chosen_mode = chosen_mode;
    costs_ctx.alt_cost_index = alt_cost_idx;

    // Exile delve cards from graveyard (cost payment); record in costs_ctx.
    let to_exile_names: Vec<String> = to_exile_ids.iter().map(|(_, n)| n.clone()).collect();
    for (exile_id, _) in &to_exile_ids {
        change_zone(*exile_id, ZoneId::Exile, state, t, who);
        costs_ctx.objects_moved.push(*exile_id);
    }

    let delve_label = if to_exile_names.is_empty() {
        String::new()
    } else {
        format!(", delve: {}", to_exile_names.join(", "))
    };
    let target_label = if !chosen_targets.is_empty() {
        let names: Vec<String> = chosen_targets.iter()
            .map(|&tid| {
                let name = state.stack_item_display_name(tid);
                if name.is_empty() { format!("<?obj#{}>", tid.0) } else { name.to_string() }
            })
            .collect();
        format!(" targeting {}", names.join(", "))
    } else {
        String::new()
    };
    state.log(t, who, format!("Cast {} ({}{}){} [hand: {}]", name, cast_label, delve_label, target_label, state.hand_size(who)));

    let (_spell_target_spec, spell_eff) = build_spell_effect(&def, who, card_id, chosen_x, chosen_mode);
    let spell_chosen_targets = chosen_targets.to_vec();

    if let Some(card) = state.objects.get_mut(&card_id) {
        card.role = ObjectRole::StackSpell(SpellState {
            effect: Some(spell_eff),
            chosen_targets: spell_chosen_targets,
            is_back_face: false,
            costs_paid_ctx: costs_ctx,
        });
    }

    // Replicate (CR 702.58): for each time the replicate cost was paid, push a copy.
    let rep_cost = def.additional_costs.replicate_mana_cost();
    if let Some(rep_mc) = rep_cost {
        // Count other valid targets for copies (different from the original target).
        let original_targets = chosen_targets.to_vec();
        let extra_targets: Vec<ObjId> = legal_targets(def.target_spec(), who, card_id, state)
            .into_iter()
            .filter(|id| !original_targets.contains(id))
            .collect();
        let mut rep_count = 0u32;
        for &tgt in &extra_targets {
            if !state.potential_mana(who).can_pay(&rep_mc) { break; }
            run_mana_loop(state, t, who, &rep_mc);
            state.player_mut(who).pool.spend(&rep_mc);
            let (_, copy_eff) = build_spell_effect(&def, who, card_id, chosen_x, chosen_mode);
            let copy_id = state.alloc_id();
            state.insert_stack_ability(copy_id, format!("{} (replicate)", name), who, AbilityState {
                effect: copy_eff,
                chosen_targets: vec![tgt],
                costs_paid_ctx: CostsPaidCtx::default(),
                is_triggered: false,
                counterable: true,
                choice_spec: None,
            });
            rep_count += 1;
            let tgt_name = state.stack_item_display_name(tgt).to_string();
            state.log(t, who, format!("Replicate → {} (targeting {})", name, tgt_name));
        }
        if rep_count > 0 {
            if let Some(card_obj) = state.objects.get_mut(&card_id) {
                if let Some(spell) = card_obj.spell_mut() {
                    spell.costs_paid_ctx.replicate_count = rep_count;
                }
            }
        }
    }

    // Latent spell mods (CR 611.2f): consume the first matching mod for this caster.
    consume_latent_spell_mod(state, who, card_id);

    // SpellCast fires after all costs paid and spell is on the stack.
    let mana_spent = match &alt_cost {
        None     => mana_value(def.mana_cost()) > 0,
        Some(ac) => ac.costs.includes_mana(),
    };
    // Cast-time choices recorded on the event so `Ctx::ThisCast` can read them
    // back at resolution (sunburst X, delve count) without transient ctx state.
    let delved: Vec<ObjId> = to_exile_ids.iter().map(|(id, _)| *id).collect();
    fire_event(GameEvent::SpellCast { caster: who, card_id, mana_spent, alt_cost: alt_cost.is_some(), x: chosen_x, delved }, state, t, who);


    Some(card_id)
}






// ── Keyword helpers ───────────────────────────────────────────────────────────

/// Return true if the permanent with `id` has the given keyword in the materialized (CE-applied) view.
/// Always reads from materialized state so CEs that grant or remove keywords are respected.
pub fn creature_has_keyword(id: ObjId, kw: Keyword, state: &SimState) -> bool {
    state.def_of(id)
        .map(|d| d.has_keyword(kw))
        .unwrap_or(false)
}

/// Which of the two combat-damage steps a damage pass is running.
/// CR 510.5 splits combat damage when first strike or double strike is involved.
#[derive(Copy, Clone, PartialEq, Debug)]
enum DamageStepKind { FirstStrike, Regular }

/// CR 510.5 / 702.4b: who deals damage in this step.
/// FS step:  first strike or double strike sources only.
/// Regular:  double strike sources, plus any source without first strike.
fn source_eligible_for_damage_step(id: ObjId, state: &SimState, step: DamageStepKind) -> bool {
    let fs = creature_has_keyword(id, Keyword::FirstStrike, state);
    let ds = creature_has_keyword(id, Keyword::DoubleStrike, state);
    match step {
        DamageStepKind::FirstStrike => fs || ds,
        DamageStepKind::Regular     => ds || !fs,
    }
}

/// CR 510.5: a first-strike combat damage step occurs only when at least one
/// attacking or blocking creature has first strike or double strike.
fn any_combatant_has_first_or_double_strike(state: &SimState) -> bool {
    let has_fs_or_ds = |id: ObjId| {
        creature_has_keyword(id, Keyword::FirstStrike, state)
        || creature_has_keyword(id, Keyword::DoubleStrike, state)
    };
    state.combat_attackers.iter().any(|&id| has_fs_or_ds(id))
        || state.combat_blocks.iter().any(|&(_, b)| has_fs_or_ds(b))
}

/// Validate a strategy's combat-damage assignment against CR 510.1c (lethal-in-order rule).
/// `raw[i]` is the strategy-proposed damage to ordered_blockers[i]; `lethal[i]` is the engine-
/// computed minimum needed for that blocker to be considered lethally damaged (deathtouch
/// already folded in by the caller). Returns a normalized assignment; if `raw` violates the
/// rule (or has the wrong length, negatives, or oversized sum), falls back to the default
/// "lethal-in-order, dump-rest-on-last (or trample-spillover)" heuristic.
fn validate_assignment(raw: &[i32], lethal: &[i32], total: i32, has_trample: bool) -> Vec<i32> {
    let n = lethal.len();
    let default = || {
        let mut out = vec![0i32; n];
        let mut remaining = total.max(0);
        for i in 0..n {
            if remaining <= 0 { break; }
            let take = remaining.min(lethal[i].max(0));
            out[i] = take;
            remaining -= take;
        }
        if !has_trample && remaining > 0 && n > 0 {
            out[n - 1] += remaining;
        }
        out
    };
    if raw.len() != n { return default(); }
    if raw.iter().any(|&x| x < 0) { return default(); }
    let sum: i32 = raw.iter().sum();
    if sum > total { return default(); }
    // Without trample, every point of damage must land on a blocker.
    if !has_trample && sum < total { return default(); }
    // CR 510.1c: a blocker behind the current one can only receive damage if every earlier
    // blocker has already been assigned at least lethal damage.
    let mut earlier_full = true;
    for i in 0..n {
        if !earlier_full && raw[i] > 0 { return default(); }
        if raw[i] < lethal[i] { earlier_full = false; }
    }
    raw.to_vec()
}


// ── Engine invariant assertions ──────────────────────────────────────────────

/// Dump the full game log to stderr then panic. Used by `check!` below so that
/// invariant failures always come with enough context to debug.
#[cfg(debug_assertions)]
macro_rules! check {
    ($cond:expr, $state:expr, $label:expr, $($arg:tt)*) => {
        if !$cond {
            eprintln!("\n╔══ ENGINE INVARIANT VIOLATION ══╗");
            eprintln!("{}", $state);
            panic!("[{}] {}", $label, format!($($arg)*));
        }
    };
}

#[cfg(not(debug_assertions))]
macro_rules! check {
    ($cond:expr, $state:expr, $label:expr, $($arg:tt)*) => { };
}

/// Comprehensive state consistency check. All checks use `check!` so they
/// dump the game log before panicking, then compile away in release builds.
/// Called at engine lifecycle boundaries.
#[cfg(debug_assertions)]
fn assert_engine_invariants(state: &SimState, label: &str) {
    // ── Stack/zone consistency: no stale stack objects ──────────────────
    // Every ID in state.stack must be an object with zone==Stack (a spell, or a
    // card-less ability object).
    for &id in &state.stack {
        let in_objects = state.objects.get(&id)
            .map_or(false, |o| o.in_zone(Zone::Stack));
        check!(in_objects, state, label,
            "stack item {id:?} not an object with zone==Stack");
    }
    // Every object with zone==Stack must be in state.stack (no stranded spells).
    for (&id, obj) in &state.objects {
        if obj.in_zone(Zone::Stack) {
            check!(state.stack.contains(&id), state, label,
                "object {:?} has zone==Stack but is not in state.stack", obj.catalog_key);
        }
    }

    // ── Zone-tracking arrays match actual zones ────────────────────────
    for &id in &state.graveyard_order {
        check!(
            state.objects.get(&id).map_or(false, |o| o.in_zone(Zone::Graveyard)),
            state, label,
            "graveyard_order contains {id:?} but object zone is not Graveyard");
    }
    for who in [PlayerId::Us, PlayerId::Opp] {
        for &id in &state.player(who).library_order {
            check!(
                state.objects.get(&id).map_or(false, |o| o.in_zone(Zone::Library)),
                state, label,
                "library_order contains {id:?} but object zone is not Library");
        }
    }
    // Reverse: every Library-zone object should be in its owner's library_order.
    for obj in state.objects.values() {
        if obj.in_zone(Zone::Library) {
            check!(
                state.player(obj.owner).library_order.contains(&obj.id),
                state, label,
                "object {:?} has zone==Library but is not in library_order", obj.catalog_key);
        }
    }

    // ── Battlefield objects have BattlefieldState and vice versa ────────
    for obj in state.objects.values() {
        if obj.in_zone(Zone::Battlefield) {
            check!(obj.bf().is_some(), state, label,
                "object {:?} on battlefield without BattlefieldState", obj.catalog_key);
        } else {
            check!(obj.bf().is_none(), state, label,
                "object {:?} in zone {:?} has stale BattlefieldState", obj.catalog_key, obj.zone());
        }
    }

    // ── Every card object has a catalog entry (skip card-less players/abilities) ──
    for obj in state.objects.values() {
        if obj.player_state().is_some() || obj.ability().is_some() { continue; }
        check!(state.catalog.contains_key(&obj.catalog_key), state, label,
            "object {:?} has no catalog entry", obj.catalog_key);
    }

    // ── Life totals are sane ───────────────────────────────────────────
    check!(state.player(PlayerId::Us).life > -100 && state.player(PlayerId::Us).life < 200, state, label,
        "Us life out of sane range: {}", state.player(PlayerId::Us).life);
    check!(state.player(PlayerId::Opp).life > -100 && state.player(PlayerId::Opp).life < 200, state, label,
        "Opp life out of sane range: {}", state.player(PlayerId::Opp).life);
}

#[cfg(not(debug_assertions))]
#[inline(always)]
fn assert_engine_invariants(_state: &SimState, _label: &str) {}

/// True iff this object is a Saga (CR 714) — carries one or more chapter abilities.
fn obj_is_saga(state: &SimState, id: ObjId) -> bool {
    state.objects.get(&id)
        .and_then(|o| state.catalog.get(&o.catalog_key))
        .map_or(false, |d| !d.chapters.is_empty())
}

/// CR 714: put a lore counter on a Saga, then fire the chapter ability whose
/// number equals the new lore count (as a triggered ability, onto the stack via
/// `pending_triggers`). Called when a Saga enters and as its controller's
/// precombat main phase begins. Once the count exceeds the final chapter there's
/// no chapter to fire; the SBA in `check_state_based_actions` then sacrifices it.
fn add_lore_counter(state: &mut SimState, saga_id: ObjId, t: u8) {
    let (controller, catalog_key, new_count) = {
        let Some(obj) = state.objects.get_mut(&saga_id) else { return };
        let c = obj.counters.entry(CounterType::Lore).or_insert(0);
        *c += 1;
        (obj.controller, obj.catalog_key.clone(), *c as usize)
    };
    let (chapter, chapter_target_spec) = match state.catalog.get(&catalog_key) {
        Some(def) => match def.chapters.get(new_count - 1) {
            Some(ch) => (
                ch.clone(),
                def.chapter_target_specs.get(new_count - 1).cloned().unwrap_or(TargetSpec::None),
            ),
            None => return,
        },
        None => return,
    };
    // The chapter ability resolves with the Saga as its source (CR 714.3).
    let effect = crate::effects::Effect(std::sync::Arc::new(
        move |state: &mut SimState, _t: u8, targets: &[ObjId]| {
            use crate::ir::expr::Value;
            let mut env = crate::ir::executor::BindEnv::new()
                .with_source(saga_id)
                .with_controller(controller);
            if let Some(&target) = targets.first() {
                let value = if state.is_player(target) {
                    Value::Player(state.who_pid(target))
                } else {
                    Value::Obj(target)
                };
                env = env.with_var("target", value);
            }
            let _ = crate::ir::executor::execute(&chapter, state, &env);
        },
    ));
    let source_name = state.permanent_name(saga_id).unwrap_or(catalog_key);
    state.log(t, controller, format!("{}: lore {} → chapter {}", source_name, new_count, new_count));
    state.pending_triggers.push(TriggerContext {
        source_name,
        controller,
        target_spec: chapter_target_spec,
        effect,
    });
}

/// Check and apply all State-Based Actions (rule 704). Called before every priority grant.
/// Runs in a loop until no SBA fires in a pass — the rules require repeated checking until stable.
fn check_state_based_actions(
    state: &mut SimState,
    t: u8,
) {
    // Ensure materialized state is current before reading it for SBA checks.
    // (It may be stale if state was mutated outside fire_event, e.g. directly in tests.)
    recompute(state);

    loop {
        let mut any = false;

        // Player-loss SBAs apply together: zero life (704.5a) or an
        // unreplaced empty-library draw (704.5b). Both losing is a draw.
        let loses = |who| state.life_of(who) <= 0 || state.player(who).failed_draw;
        let us_loses = loses(PlayerId::Us);
        let opp_loses = loses(PlayerId::Opp);
        if us_loses || opp_loses {
            for (who, lost) in [(PlayerId::Us, us_loses), (PlayerId::Opp, opp_loses)] {
                if !lost { continue; }
                let reason = if state.player(who).failed_draw {
                    "attempted to draw from an empty library".to_string()
                } else {
                    format!("life: {}", state.life_of(who))
                };
                state.log(t, who, format!("→ loses the game ({reason})"));
            }
            state.winner = match (us_loses, opp_loses) {
                (true, false) => Some(PlayerId::Opp),
                (false, true) => Some(PlayerId::Us),
                _ => None,
            };
            state.terminal = true;
            return;
        }

        // SBA: token in a zone other than the battlefield ceases to exist (rule 704.5d).
        let dead_tokens: Vec<ObjId> = state.objects.values()
            .filter(|c| c.is_token && !c.in_zone(Zone::Battlefield))
            .map(|c| c.id)
            .collect();
        for id in dead_tokens {
            state.objects.remove(&id);
            state.graveyard_order.retain(|&x| x != id);
            any = true;
        }
        check!(
            state.objects.values().all(|c| !c.is_token || c.in_zone(Zone::Battlefield)),
            state, "sba",
            "token found outside battlefield after SBA cleanup"
        );

        // SBA: creature with toughness ≤ 0 goes to graveyard (rule 704.5f).
        // SBA: creature with toughness ≤ 0 ceases to exist (CR 704.5f) — not "destroyed",
        // so indestructible does not apply; use change_zone directly.
        // SBA: creature with lethal damage is destroyed (CR 704.5g) — indestructible applies;
        // use destroy_one so indestructibility checks there will fire when added.
        for who in [PlayerId::Us, PlayerId::Opp] {
            let mut zero_tgh: Vec<ObjId> = Vec::new();
            let mut lethal_dmg: Vec<ObjId> = Vec::new();
            for card in state.permanents_of(who).collect::<Vec<_>>() {
                let Some(bf) = card.bf() else { continue };
                if !state.def_of(card.id).map_or(false, |d| d.is_creature()) { continue; }
                let tgh = state.def_of(card.id)
                    .and_then(|d| d.as_creature())
                    .map(|c| c.toughness())
                    .unwrap_or(1);
                if tgh <= 0 { zero_tgh.push(card.id); }
                else if bf.damage >= tgh { lethal_dmg.push(card.id); }
            }
            for id in zero_tgh {
                change_zone(id, ZoneId::Graveyard, state, t, who);
                any = true;
            }
            for id in lethal_dmg {
                destroy_one(id, state, t, who);
                any = true;
            }
        }

        // SBA: planeswalker with loyalty ≤ 0 goes to graveyard (rule 704.5i).
        for who in [PlayerId::Us, PlayerId::Opp] {
            let dying: Vec<ObjId> = state.permanents_of(who)
                .filter_map(|card| {
                    let bf = card.bf()?;
                    if !state.def_of(card.id).map_or(false, |d| matches!(d.kind, CardKind::Planeswalker(_))) { return None; }
                    if bf.loyalty <= 0 { Some(card.id) } else { None }
                })
                .collect();
            for id in dying {
                change_zone(id, ZoneId::Graveyard, state, t, who);
                any = true;
            }
        }

        // SBA: a Saga with lore counters ≥ its final chapter number is sacrificed
        // (CR 714.4). The final chapter ability was pushed onto the stack when the
        // lore count reached it and resolves independently of its source, so we
        // don't gate on "chapter still on the stack" — correct as long as no
        // chapter references the Saga after this point (true for current sagas).
        for who in [PlayerId::Us, PlayerId::Opp] {
            let done_sagas: Vec<ObjId> = state.permanents_of(who)
                .filter_map(|card| {
                    let chapters = state.def_of(card.id).map(|d| d.chapters.len()).unwrap_or(0);
                    if chapters == 0 { return None; }
                    let lore = card.counters.get(&CounterType::Lore).copied().unwrap_or(0) as usize;
                    if lore >= chapters { Some(card.id) } else { None }
                })
                .collect();
            for id in done_sagas {
                change_zone(id, ZoneId::Graveyard, state, t, who);
                any = true;
            }
        }

        // SBA: legend rule — if a player controls two or more legendary permanents with the
        // same name, that player chooses one to keep; the rest go to graveyard (rule 704.5j).
        for who in [PlayerId::Us, PlayerId::Opp] {
            // Collect (name, id) for all legendary permanents controlled by `who`.
            let mut seen: HashMap<String, ObjId> = HashMap::new();
            let mut extras: Vec<ObjId> = Vec::new();
            let legendaries: Vec<(String, ObjId)> = state.permanents_of(who)
                .filter(|card| {
                    state.def_of(card.id)
                        .map_or(false, |d| d.legendary())
                })
                .map(|card| (card.catalog_key.clone(), card.id))
                .collect();
            for (name, id) in legendaries {
                if let Some(_existing) = seen.get(&name) {
                    extras.push(id); // keep the first one, sacrifice the later one
                } else {
                    seen.insert(name, id);
                }
            }
            for id in extras {
                change_zone(id, ZoneId::Graveyard, state, t, who);
                any = true;
            }
        }

        if !any { break; }
    }
}

pub(crate) fn do_create_token(token_key: &str, controller: PlayerId, state: &mut SimState, t: u8) -> ObjId {
    let new_id = state.alloc_id();
    state.objects.insert(new_id, GameObject {
        id: new_id,
        catalog_key: token_key.to_string(),
        owner: controller,
        controller,
        is_token: true,
        role: ObjectRole::Battlefield(BattlefieldState::new()),
        materialized: None,
        counters: HashMap::new(), ci_timestamp: 0,
    });
    {
        let ts = state.next_ci_timestamp();
        if let Some(obj) = state.objects.get_mut(&new_id) { obj.ci_timestamp = ts; }
    }
    state.log(t, controller, format!("{token_key} created"));
    // Notify the event pipeline (objectives observe this for token-making
    // wincons; no replacement/trigger in this deck reacts to it).
    fire_event(
        GameEvent::TokenCreated { controller, token_key: token_key.to_string(), id: new_id },
        state, t, controller,
    );
    new_id
}

/// Pop and resolve the top item of the stack.
///
/// If the top id is in `state.objects` it is a spell: runs its effect and moves the card to
/// graveyard (instant/sorcery) or exile-on-adventure, or leaves zone management to
/// `eff_enter_permanent` (permanent spells). If the id is in `state.abilities` it is an
/// activated or triggered ability: runs its effect and removes the entry.
fn resolve_top_of_stack(
    state: &mut SimState,
    t: u8,
    _ap: PlayerId,
) {
    // CR 608.2m: a spell/ability stays on the stack while it resolves; it leaves
    // the stack only as the FINAL step of resolution (to the graveyard / battlefield,
    // or it ceases to exist). Peek here and let each branch remove it *after* its
    // effect runs — so effects that read the stack or count the graveyard (Flow
    // State's instant∧sorcery check, delve, threshold) see the correct state and do
    // not observe the resolving object as already in the graveyard.
    let id = *state.stack.last().unwrap();
    let is_ability = state.objects.get(&id).map_or(false, |o| o.ability().is_some());
    if is_ability {
        // Card-less stack object: an activated/triggered ability resolves, then
        // ceases to exist (CR 608.2m). Clone its payload (Effect is an Arc — cheap)
        // but LEAVE the object on the stack while the effect runs; remove it after.
        let (effect, chosen_targets, choice_spec, costs_paid_ctx, controller) = {
            let obj = state.objects.get(&id).expect("ability object on stack");
            let controller = obj.controller;
            let ObjectRole::StackAbility(ability) = &obj.role else {
                panic!("ability object carries an ability payload");
            };
            (ability.effect.clone(), ability.chosen_targets.clone(),
             ability.choice_spec.clone(), ability.costs_paid_ctx.clone(), controller)
        };
        let mut effect_targets = chosen_targets;
        if let Some(ref spec) = choice_spec {
            let choices = enumerate_choices(spec, controller, state);
            if let Some(chosen) = state.with_strategy(controller, |s, st| s.choose_for_effect(id, &choices, st)) {
                effect_targets.insert(0, chosen);
            }
        }
        // Make costs_paid_ctx visible to the effect closure (e.g. ninjutsu reads attack_target).
        state.resolving_costs_ctx = costs_paid_ctx;
        effect.call(state, t, &effect_targets);
        state.resolving_costs_ctx = CostsPaidCtx::default();
        // Resolution complete → the ability leaves the stack and ceases to exist.
        state.stack.retain(|&x| x != id);
        state.objects.remove(&id);
    } else if state.objects.contains_key(&id) {
        // It's a spell (card on the stack)
        let spell = state.objects[&id].spell().cloned().unwrap_or_else(|| SpellState {
            effect: None,
            chosen_targets: vec![],
            is_back_face: false,
            costs_paid_ctx: CostsPaidCtx::default(),
        });
        let owner = state.objects[&id].owner;
        let controller = state.objects[&id].controller;
        let name = state.objects[&id].catalog_key.clone();

        // Back face of a split card whose back has subtype "adventure" → exile to on_adventure.
        let is_adventure = spell.is_back_face
            && state.catalog.get(name.as_str())
                .and_then(|d| d.back.as_ref())
                .map_or(false, |b| b.has_subtype("adventure"));

        if is_adventure {
            if let Some(ref eff) = spell.effect {
                eff.call(state, t, &spell.chosen_targets);
            }
            // Resolved → leaves the stack (exiled on adventure).
            state.stack.retain(|&x| x != id);
            let back_name = state.catalog.get(name.as_str())
                .and_then(|d| d.back.as_ref())
                .map(|b| b.name.as_str())
                .unwrap_or(name.as_str())
                .to_string();
            if let Some(card_obj) = state.objects.get_mut(&id) {
                card_obj.role = ObjectRole::Exile { on_adventure: true };
            }
            state.log(t, owner, format!("{} resolves → {} on adventure in exile", back_name, name));
        } else if let Some(ref eff) = spell.effect {
            let is_perm = state.def_of(id)
                .map(|d| matches!(d.kind, CardKind::Creature(_) | CardKind::Artifact(_)
                    | CardKind::Planeswalker(_) | CardKind::Enchantment(_)))
                .unwrap_or(false);
            if !is_perm {
                state.log(t, owner, format!("{} resolves", name));
                // The effect runs WHILE the spell is still on the stack (CR 608.2m):
                // graveyard-counting effects (Flow State, delve, threshold) must not
                // see this spell in the graveyard — it isn't there yet.
                eff.call(state, t, &spell.chosen_targets);
                // Final resolution step: it leaves the stack for the graveyard.
                state.stack.retain(|&x| x != id);
                change_zone(id, ZoneId::Graveyard, state, t, owner);
            } else {
                // A permanent spell becomes the permanent: it leaves the stack and
                // enters the battlefield.
                state.stack.retain(|&x| x != id);
                // Stash costs-paid ctx so ETB replacement effects (e.g. Murktide) can read it.
                state.resolving_costs_ctx = spell.costs_paid_ctx.clone();
                // Move the spell object from Stack → Battlefield (same object, no
                // new allocation). This is how MTG works: the spell becomes the
                // permanent. `change_zone` → `set_zone(Battlefield)` drops the
                // spell payload and fires the ETB event for triggers.
                change_zone(id, ZoneId::Battlefield, state, t, owner);
                state.log(t, owner, format!("{} enters play", name));
                // Don't call eff here: permanent spell effects are always
                // eff_enter_permanent (spell_modes() returns None for all
                // permanent types), and the zone transition above replaces it.
                state.resolving_costs_ctx = CostsPaidCtx::default();
            }
        } else {
            state.log(t, owner, format!("{} resolves", name));
            state.stack.retain(|&x| x != id);
            change_zone(id, ZoneId::Graveyard, state, t, owner);
        }

        // The spell finished resolving (effect applied, or it became a permanent).
        // Fire the general resolution event so objectives / triggers can react.
        fire_event(GameEvent::SpellResolved { controller, card_id: id }, state, t, controller);
    }
}

/// Cast sub-machine (CR 601.2a-i).
///
/// Drives through Announce → Targets → LegalCheck → ComputeCost → ActivateMana → PayCosts → Complete.
/// Strategy callbacks drive each decision point.
fn run_cast_submachine(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    card_id: ObjId,
    face: SpellFace,
) -> Option<ObjId> {
    // ── Announce (CR 601.2b) ────────────────────────────────────────────
    let def = state.def_of(card_id)
        .cloned()
        .or_else(|| {
            let key = &state.objects.get(&card_id)?.catalog_key;
            state.catalog.get(key.as_str()).cloned()
        });
    let def = def?;

    let max_x = max_affordable_x_for_spell(state, who, &def, face);
    let options = AnnounceOptions {
        available_modes: def.mode_count()
            .map(|n| (0..n).collect())
            .unwrap_or_else(|| vec![0]),
        available_alt_costs: def.alternate_costs().to_vec(),
        has_x_cost: def.additional_costs.has_x_cost(),
        max_x,
    };
    let ann = state.with_strategy(who, |s, st| s.announce(st, card_id, &options));
    let chosen_mode = ann.chosen_mode;
    let announced_alt_index = ann.alt_cost_index;
    let chosen_x = ann.chosen_x;
    if options.has_x_cost && chosen_x > options.max_x {
        return None;
    }

    // ── Targets (CR 601.2c) ─────────────────────────────────────────────
    let target_spec = if face == SpellFace::Back {
        def.back.as_ref()
            .map(|b| b.target_spec().clone())
            .unwrap_or(TargetSpec::None)
    } else {
        def.target_spec_for_mode(chosen_mode).clone()
    };
    let legal = legal_targets(&target_spec, who, card_id, state);
    // CR 601.2c: a spell that requires targets cannot be cast if no legal targets exist.
    if !target_spec.is_none() && legal.is_empty() {
        return None;
    }
    let chosen_targets = if legal.is_empty() {
        vec![]
    } else {
        state.with_strategy(who, |s, st| s.choose_targets(st, card_id, &legal, &target_spec))
    };

    // ── LegalCheck (CR 601.2e) ──────────────────────────────────────────
    // Sorcery-speed check done by caller before entering sub-machine.

    let preferred_cost = announced_alt_index
        .and_then(|i| def.alternate_costs().get(i).cloned());

    // ── ComputeCost + ActivateMana (CR 601.2f-g) ────────────────────────
    let mana_cost = if let Some(ref alt) = preferred_cost {
        let base = alt.costs.first_mana_cost().unwrap_or_default();
        apply_casting_cost_rules(base, &def)
    } else if face == SpellFace::Back {
        let def = state.def_of(card_id)
            .or_else(|| {
                let key = &state.objects.get(&card_id)?.catalog_key;
                state.catalog.get(key.as_str())
            });
        let back_cost = def.and_then(|d| d.adventure())
            .map(|a| a.mana_cost())
            .unwrap_or("");
        let base = parse_mana_cost(back_cost);
        match def {
            Some(d) => apply_casting_cost_rules(base, d),
            None => base,
        }
    } else {
        let def = state.def_of(card_id)
            .or_else(|| {
                let key = &state.objects.get(&card_id)?.catalog_key;
                state.catalog.get(key.as_str())
            });
        let base = parse_mana_cost(def.map(|d| d.mana_cost()).unwrap_or(""));
        match def {
            Some(d) => apply_casting_cost_rules(base, d),
            None => base,
        }
    };
    let mana_cost = combined_mana_cost_with_x(mana_cost, &def.additional_costs, chosen_x);
    state.casting_spell = Some(card_id);
    run_mana_loop(state, t, who, &mana_cost);

    // ── PayCosts + Complete (CR 601.2h-i) ───────────────────────────────
    // cast_spell handles remaining payment (pool already filled by mana loop),
    // zone move, effect building, and event firing.
    let pool_before_payment = state.player(who).pool.clone();
    let result = cast_spell(state, t, who, card_id, face, preferred_cost.as_ref(),
               announced_alt_index, &chosen_targets, chosen_x, chosen_mode);
    if result.is_none() {
        eprintln!("[cast-reject] {}: announcement={:?} X={}, pool_before={:?}, pool_after={:?}, zone_after={:?}",
            state.objects.get(&card_id).map(|o| o.catalog_key.as_str()).unwrap_or("?"),
            face, chosen_x, pool_before_payment, state.player(who).pool,
            state.objects.get(&card_id).and_then(|o| o.zone()));
    }
    state.casting_spell = None;
    result
}

/// Loyalty abilities require the controller's main phase, an empty stack,
/// enough loyalty to pay a negative cost, and no prior activation this turn.
pub(crate) fn loyalty_activation_allowed(
    state: &SimState, who: PlayerId, source_id: ObjId, ability: &AbilityDef,
) -> bool {
    let Some(delta) = ability.loyalty_delta() else { return true };
    state.active_player() == Some(who)
        && matches!(state.current_phase, Some(TurnPosition::Phase(
            PhaseKind::PreCombatMain | PhaseKind::PostCombatMain
        )))
        && state.stack.is_empty()
        && state.permanent_bf(source_id).map_or(false, |bf| {
            !bf.pw_activated_this_turn && bf.loyalty + delta >= 0
        })
}

/// Activate sub-machine (CR 602.2b).
///
/// Pays ability costs, builds effect, and pushes the ability onto the stack.
/// Strategy callback drives target selection.
fn run_activate_submachine(
    state: &mut SimState,
    t: u8,
    who: PlayerId,
    source_id: ObjId,
    ability: &AbilityDef,
) -> ObjId {
    if ability.loyalty_delta().is_some() && !loyalty_activation_allowed(state, who, source_id, ability) {
        return ObjId::UNSET;
    }
    // Recheck at execution too: callers may hold a stale action after the
    // source has tapped. Never spend mana or stack an unpaid activation.
    if ability.costs.requires_tap_self()
        && !state.permanent_bf(source_id).map_or(false, |bf| !bf.tapped)
    {
        return ObjId::UNSET;
    }
    // ── Targets ─────────────────────────────────────────────────────────
    let chosen_targets = {
        let legal = legal_targets(&ability.target_spec, who, source_id, state);
        // CR 602.2b: an ability that requires targets cannot be activated
        // if no legal targets exist.
        if !ability.target_spec.is_none() && legal.is_empty() {
            eprintln!("[activate] BUG: no legal targets for ability on {:?}, skipping",
                state.permanent_name(source_id).unwrap_or_default());
            check!(false, state, "activate", "BUG: ability activated with no legal targets");
            return ObjId::UNSET;
        }
        if legal.is_empty() {
            vec![]
        } else {
            state.with_strategy(who, |s, st| s.choose_targets(st, source_id, &legal, &ability.target_spec))
        }
    };

    let source_name_for_stack = state.permanent_name(source_id)
        .or_else(|| state.objects.get(&source_id).map(|c| c.catalog_key.clone()))
        .unwrap_or_default();
    let is_hand_source = matches!(ability.source_zone, SourceZone::Hand);

    // ── Mana loop (CR 602.2b) ──────────────────────────────────────────
    // Fill pool via strategy-driven mana loop before paying costs.
    // Works across both `CostBody` variants: Legacy scans for
    // `CostComponent::Mana`, Ir walks for `Action::PayMana`.
    if let Some(mc) = ability.costs.first_mana_cost() {
        run_mana_loop(state, t, who, &mc);
    }

    // ── Pay costs ───────────────────────────────────────────────────────
    let Some(ctx) = pay_ability_cost(state, t, who, source_id, ability, is_hand_source) else {
        eprintln!("[activate-reject] source={:?} name={} mana_cost={:?} pool={:?} tapped={:?} targets={:?}",
            source_id, source_name_for_stack, ability.costs.first_mana_cost(),
            state.player(who).pool, state.permanent_bf(source_id).map(|bf| bf.tapped),
            chosen_targets);
        return ObjId::UNSET;
    };

    // ── Build effect ────────────────────────────────────────────────────
    let eff = build_ability_effect(ability, who, source_id);

    // ── Push to stack ───────────────────────────────────────────────────
    let ab_id = state.alloc_id();
    state.insert_stack_ability(ab_id, source_name_for_stack, who, AbilityState {
        effect: eff,
        chosen_targets,
        costs_paid_ctx: ctx,
        is_triggered: false,
        counterable: true,
        choice_spec: ability.choice_spec.clone(),
    });

    ab_id
}

fn apply_priority_action(
    state: &mut SimState,
    t: u8,
    ap: PlayerId,
    who: PlayerId,
    chosen: LegalAction,
    priority_holder: &mut PlayerId,
    last_passer: &mut Option<PlayerId>,
) -> bool {
    let nap = ap.opp();
    match chosen {
        LegalAction::Pass => {
            let other = if who == ap { nap } else { ap };
            if *last_passer == Some(other) {
                if state.stack.is_empty() {
                    return true;
                } else {
                    resolve_top_of_stack(state, t, ap);
                    assert_engine_invariants(state, "post-resolve");
                    *priority_holder = ap;
                    *last_passer = None;
                }
            } else {
                *last_passer = Some(who);
                *priority_holder = other;
            }
        }
        LegalAction::LandDrop(card_id) => {
            sim_play_land(state, t, who, card_id);
            state.player_mut(who).lands_played_this_turn += 1;
            *priority_holder = who;
            *last_passer = None;
        }
        LegalAction::CastSpell { card_id, face } => {
            let name = state.objects.get(&card_id).map(|c| c.catalog_key.clone()).unwrap_or_default();
            let is_instant = match face {
                SpellFace::Main => state.def_of(card_id)
                    .map(|d| d.is_instant() || d.has_keyword(Keyword::Flash))
                    .unwrap_or(false),
                SpellFace::Back => state.def_of(card_id)
                    .and_then(|d| d.back.as_ref())
                    .map(|b| b.is_instant() || b.has_keyword(Keyword::Flash))
                    .unwrap_or(false),
            };
            if !is_instant && !state.stack.is_empty() {
                eprintln!("[priority] BUG: sorcery-speed {} on non-empty stack, treating as Pass", name);
                state.invalid_actions += 1;
                state.log(t, who, format!("INVALID CAST TIMING: {}", name));
                check!(false, state, "priority", "BUG: sorcery-speed cast of {} on non-empty stack", name);
                *last_passer = Some(who);
                *priority_holder = if who == ap { nap } else { ap };
            } else {
                let result = run_cast_submachine(state, t, who, card_id, face);
                if let Some(cid) = result {
                    state.stack.push(cid);
                    // CR 117.3c: the player who cast a spell receives priority again.
                    *priority_holder = who;
                    *last_passer = None;
                } else {
                    let pool = &state.player(who).pool;
                    eprintln!("[priority] BUG: cast failed for {} by {} (pool B={} U={} tot={}, hand={})",
                        name, who, pool.b, pool.u, pool.total, state.hand_size(who));
                    state.invalid_actions += 1;
                    state.log(t, who, format!("INVALID CAST: {}", name));
                    check!(false, state, "priority", "BUG: cast failed");
                    *last_passer = Some(who);
                    *priority_holder = if who == ap { nap } else { ap };
                }
            }
        }
        LegalAction::ActivateAbility { source_id, ability_index } => {
            let ab = state.def_of(source_id)
                .and_then(|d| d.abilities().get(ability_index).cloned())
                .unwrap_or_default();
            if run_activate_submachine(state, t, who, source_id, &ab) == ObjId::UNSET {
                state.invalid_actions += 1;
                // A failed attempt must not keep priority forever. Use the
                // normal pass path, including resolution after two passes.
                state.log(t, who, format!("Activation failed for {:?}; passing priority", source_id));
                return apply_priority_action(
                    state, t, ap, who, LegalAction::Pass, priority_holder, last_passer,
                );
            }
            *priority_holder = who;
            *last_passer = None;
        }
        LegalAction::ActivateManaAbility { source_id, ability_index } => {
            // Mana abilities resolve immediately and don't pass priority (CR 605.3b).
            let act = ManaActivation { source_id, ability_index, color_choice: None };
            let _ = execute_mana_activation(state, t, who, &act);
            *priority_holder = who;
        }
    }
    false
}

fn drain_priority_decisions(state: &mut SimState) {
    for p in [PlayerId::Us, PlayerId::Opp] {
        let d = state.with_strategy(p, |s, _| s.drain_decisions());
        state.decision_log.extend(d);
    }
}

fn handle_priority_round_from(
    state: &mut SimState,
    t: u8,
    ap: PlayerId,
    mut priority_holder: PlayerId,
    mut last_passer: Option<PlayerId>,
) {
    // Abort instead of returning a truncated game as valid.
    // A budget hit identifies a long priority window, not proof of a cycle;
    // the recent actions let the caller inspect whether it is making progress.
    let diagnostic_limit = std::env::var("LIBMTG_PRIORITY_LIMIT").ok()
        .map(|value| value.parse::<usize>().ok().filter(|&n| n > 0)
            .expect("LIBMTG_PRIORITY_LIMIT must be a positive integer"))
        .unwrap_or(10_000);
    let mut diagnostic_steps = 0usize;
    let mut recent_actions = std::collections::VecDeque::new();
    loop {
        let queued = std::mem::take(&mut state.pending_triggers);
        push_triggers(queued, state);
        check_state_based_actions(state, t);
        if state.done() { break; }

        let who = priority_holder;
        let legal = strategy::collect_legal_actions(state, who);
        let (chosen, decisions) = state.with_strategy(who, |s, st| {
            let c = s.choose_action(st, ap, &legal);
            (c, s.drain_decisions())
        });
        state.decision_log.extend(decisions);

        // Only format trace entries near the limit, keeping ordinary search cheap.
        if diagnostic_steps >= diagnostic_limit.saturating_sub(16) {
            let source = match &chosen {
                LegalAction::LandDrop(id) => Some(*id),
                LegalAction::CastSpell { card_id, .. } => Some(*card_id),
                LegalAction::ActivateAbility { source_id, .. }
                | LegalAction::ActivateManaAbility { source_id, .. } => Some(*source_id),
                LegalAction::Pass => None,
            };
            let name = source.and_then(|id| state.objects.get(&id))
                .map(|obj| obj.catalog_key.as_str()).unwrap_or("-");
            let pool = &state.player(who).pool;
            recent_actions.push_back(format!(
                "step={} player={:?} action={:?} card={} stack={} mana(WUBRGC,total)={:?} hand={} life={}",
                diagnostic_steps + 1, who, chosen, name, state.stack.len(),
                (pool.w, pool.u, pool.b, pool.r, pool.g, pool.c, pool.total),
                state.hand_size(who), state.life_of(who),
            ));
            if recent_actions.len() > 16 { recent_actions.pop_front(); }
            if diagnostic_steps >= diagnostic_limit {
                panic!("[priority-limit] exceeded {} actions in one window: turn={} phase={:?}\n{}",
                    diagnostic_limit, t, state.current_phase,
                    recent_actions.iter().cloned().collect::<Vec<_>>().join("\n"));
            }
        }
        diagnostic_steps += 1;

        let round_done = apply_priority_action(
            state, t, ap, who, chosen, &mut priority_holder, &mut last_passer,
        );
        drain_priority_decisions(state);

        if round_done || state.done() {
            break;
        }
    }
    // CR 117.4: priority round ends only when both players pass in succession
    // with an empty stack (or the game ends).
    check!(state.stack.is_empty() || state.done(), state, "priority-round-end",
        "priority round ended with non-empty stack");
    assert_engine_invariants(state, "priority-round-end");
}

fn handle_priority_round(
    state: &mut SimState,
    t: u8,
    ap: PlayerId,
) {
    handle_priority_round_from(state, t, ap, ap, None);
}

/// Search helper: fork `state`, force one already-legal priority action, then
/// let caller-supplied continuation strategies finish the current priority
/// window. It does not advance to later phases or turns, and the priority
/// action budget aborts runaway continuations. It compares sequencing lines
/// cheaply and avoids
/// making the entire game loop re-entrant from inside `Strategy::choose_action`.
///
/// `rollout_seed` drives all random effects that occur after the branch point.
pub fn simulate_priority_action(
    state: &SimState,
    ap: PlayerId,
    who: PlayerId,
    action: LegalAction,
    rollout_seed: u64,
    us_continuation: Box<dyn Strategy>,
    opp_continuation: Box<dyn Strategy>,
) -> SimState {
    let mut branch = state.fork_for_search(rollout_seed);
    branch.set_strategy(PlayerId::Us, us_continuation);
    branch.set_strategy(PlayerId::Opp, opp_continuation);

    let t = branch.current_turn;
    let mut priority_holder = who;
    let mut last_passer = None;
    let round_done = apply_priority_action(
        &mut branch,
        t,
        ap,
        who,
        action,
        &mut priority_holder,
        &mut last_passer,
    );
    drain_priority_decisions(&mut branch);
    if !round_done && !branch.done() {
        handle_priority_round_from(
            &mut branch,
            t,
            ap,
            priority_holder,
            last_passer,
        );
    }
    branch
}

/// One pass of combat damage (CR 510). `step` selects whether sources with first
/// strike / double strike act (FirstStrike pass) or whether normal sources act
/// (Regular pass). Both phases — attacker→blockers and blockers→attacker — happen
/// simultaneously within a single pass; SBA between passes (handled by the step
/// driver) cleans up any creatures that died from first-strike damage.
fn do_combat_damage_pass(
    state: &mut SimState,
    t: u8,
    ap: PlayerId,
    step: DamageStepKind,
) {
    if state.combat_attackers.is_empty() { return; }
    let nap = ap.opp();
    let attackers   = state.combat_attackers.clone();
    let block_pairs = state.combat_blocks.clone();

    let alive = |id: ObjId, state: &SimState| -> bool {
        state.objects.get(&id).map(|o| o.in_zone(Zone::Battlefield)).unwrap_or(false)
    };

    // Group blockers per attacker, preserving combat_blocks order (CR 509.1h).
    // Filter to live blockers — anything killed by the prior FS pass is already gone.
    let mut blocks_by_atk: HashMap<ObjId, Vec<ObjId>> = HashMap::new();
    let mut was_blocked: std::collections::HashSet<ObjId> = Default::default();
    for &(a, b) in &block_pairs {
        was_blocked.insert(a);
        if alive(b, state) {
            blocks_by_atk.entry(a).or_default().push(b);
        }
    }

    let pt = |id: ObjId, state: &SimState| -> (i32, i32) {
        state.def_of(id).and_then(|d| d.as_creature())
            .map(|c| (c.power(), c.toughness())).unwrap_or((1, 1))
    };

    let mut player_damage = 0i32;
    let mut pw_damage: HashMap<ObjId, i32> = HashMap::new();
    // Damage actually dealt by each source this pass (drives lifelink life gain).
    let mut dealt: HashMap<ObjId, i32> = HashMap::new();

    // Phase 1: each attacker assigns its damage.
    for &atk_id in &attackers {
        if !alive(atk_id, state) { continue; }
        if !source_eligible_for_damage_step(atk_id, state, step) { continue; }
        let (atk_pow, _) = pt(atk_id, state);
        if atk_pow <= 0 { continue; }
        let atk_dt      = creature_has_keyword(atk_id, Keyword::Deathtouch, state);
        let atk_trample = creature_has_keyword(atk_id, Keyword::Trample, state);

        if was_blocked.contains(&atk_id) {
            let blockers = blocks_by_atk.get(&atk_id).cloned().unwrap_or_default();
            if blockers.is_empty() {
                // CR 702.19c: a trample attacker whose blockers all left combat dumps
                // its damage to the defender. Without trample, no damage is dealt.
                if atk_trample {
                    let target = state.objects.get(&atk_id)
                        .and_then(|p| p.bf()).and_then(|bf| bf.attack_target);
                    match target {
                        None => player_damage += atk_pow,
                        Some(pw) => *pw_damage.entry(pw).or_insert(0) += atk_pow,
                    }
                    *dealt.entry(atk_id).or_insert(0) += atk_pow;
                }
                continue;
            }
            // Compute lethal-per-blocker so the strategy can reason about it.
            // CR 702.2c (deathtouch): any nonzero damage is lethal.
            let lethal_per_blocker: Vec<i32> = blockers.iter().map(|&b| {
                let (_, blk_tgh) = pt(b, state);
                let blk_existing = state.permanent_bf(b).map(|bf| bf.damage).unwrap_or(0);
                if atk_dt { 1 } else { (blk_tgh - blk_existing).max(1) }
            }).collect();

            // Strategy chooses the assignment; validator ensures CR 510.1c.
            let raw = state.with_strategy(ap, |s, st|
                s.assign_combat_damage(st, atk_id, &blockers, atk_pow,
                                       &lethal_per_blocker, atk_trample));
            let assignment = validate_assignment(
                &raw, &lethal_per_blocker, atk_pow, atk_trample);

            let mut remaining = atk_pow;
            for (i, &blk_id) in blockers.iter().enumerate() {
                let assign = assignment[i];
                if assign > 0 && !is_protected_from(blk_id, atk_id, state) {
                    let (_, blk_tgh) = pt(blk_id, state);
                    if let Some(bf) = state.permanent_bf_mut(blk_id) {
                        if atk_dt {
                            bf.damage = bf.damage.max(blk_tgh);
                        } else {
                            bf.damage += assign;
                        }
                    }
                    *dealt.entry(atk_id).or_insert(0) += assign;
                }
                remaining -= assign;
            }
            // CR 702.19b (trample): leftover damage spills to defending player/PW.
            if atk_trample && remaining > 0 {
                let target = state.objects.get(&atk_id)
                    .and_then(|p| p.bf()).and_then(|bf| bf.attack_target);
                match target {
                    None => player_damage += remaining,
                    Some(pw) => *pw_damage.entry(pw).or_insert(0) += remaining,
                }
                *dealt.entry(atk_id).or_insert(0) += remaining;
            }
        } else {
            // Unblocked.
            let target = state.objects.get(&atk_id)
                .and_then(|p| p.bf()).and_then(|bf| bf.attack_target);
            match target {
                None => player_damage += atk_pow,
                Some(pw) => *pw_damage.entry(pw).or_insert(0) += atk_pow,
            }
            *dealt.entry(atk_id).or_insert(0) += atk_pow;
        }
    }

    // Phase 2: blockers deal damage to their attacker.
    for &(atk_id, blk_id) in &block_pairs {
        if !alive(blk_id, state) { continue; }
        // CR 510.1d: a blocker whose attacker has left the battlefield deals no damage.
        if !alive(atk_id, state) { continue; }
        if !source_eligible_for_damage_step(blk_id, state, step) { continue; }
        let (blk_pow, _) = pt(blk_id, state);
        if blk_pow <= 0 { continue; }
        let blk_dt = creature_has_keyword(blk_id, Keyword::Deathtouch, state);
        let (_, atk_tgh) = pt(atk_id, state);
        if !is_protected_from(atk_id, blk_id, state) {
            if let Some(bf) = state.permanent_bf_mut(atk_id) {
                if blk_dt {
                    bf.damage = bf.damage.max(atk_tgh);
                } else {
                    bf.damage += blk_pow;
                }
            }
            *dealt.entry(blk_id).or_insert(0) += blk_pow;
        }
    }

    // CR 702.15b (lifelink): damage dealt by a lifelink source causes its
    // controller to gain that much life.
    let lifelink_gains: Vec<(PlayerId, ObjId, i32)> = dealt.iter()
        .filter(|(_, &amt)| amt > 0)
        .filter(|(&id, _)| creature_has_keyword(id, Keyword::Lifelink, state))
        .filter_map(|(&id, &amt)| {
            state.objects.get(&id).map(|o| (o.controller, id, amt))
        })
        .collect();
    for (ctlr, src_id, amt) in lifelink_gains {
        state.gain_life(ctlr, amt);
        let name = state.permanent_name(src_id).unwrap_or_default();
        let life = state.life_of(ctlr);
        state.log(t, ctlr, format!("Lifelink: {} gains {} life from {} (life: {})", ctlr, amt, name, life));
    }

    if player_damage > 0 {
        if !state.player(nap).protection_from_everything {
            state.lose_life(nap, player_damage);
        }
        state.log(t, ap, format!("Combat: {} damage to {} (life: {})", player_damage, nap, state.life_of(nap)));
    }
    for (&pw_id, &dmg) in &pw_damage {
        let new_loyalty = if let Some(bf) = state.permanent_bf_mut(pw_id) {
            bf.loyalty -= dmg;
            Some(bf.loyalty)
        } else {
            None
        };
        if let Some(new_loyalty) = new_loyalty {
            let pw_name = state.permanent_name(pw_id).unwrap_or_default();
            state.log(t, ap, format!("Combat: {} damage to {} (loyalty: {})", dmg, pw_name, new_loyalty));
        }
    }
}

/// Execute a single step: apply automatic effects, then optionally run a priority round.
fn do_step(
    state: &mut SimState,
    t: u8,
    ap: PlayerId,
    step: &Step,
    on_play: bool,
) {
    // Ensure materialized state is current at the start of every step.
    // Strategy calls (declare_attackers, declare_blockers) and combat damage run against
    // this snapshot; fire_event also rebuilds it after each tick.
    recompute(state);
    assert_engine_invariants(state, "step-start");

    state.current_phase = Some(TurnPosition::Step(step.kind));
    match step.kind {
        StepKind::Untap => {
            let perm_ids: Vec<ObjId> = state.permanents_of(ap).map(|c| c.id).collect();
            for id in perm_ids {
                let untaps_normally = state.def_of(id)
                    .map_or(true, |d| d.untaps_during_untap_step);
                if let Some(bf) = state.permanent_bf_mut(id) {
                    // CR 122.1d: stun counters replace an untap that would happen.
                    if untaps_normally {
                        if bf.stun_counters > 0 && bf.tapped {
                            bf.stun_counters -= 1;
                        } else {
                            bf.tapped = false;
                        }
                    }
                    bf.entered_this_turn = false;
                    bf.pw_activated_this_turn = false;
                }
            }
            state.player_mut(ap).lands_played_this_turn = 0;
            state.player_mut(ap).draws_this_turn = 0;
            state.player_mut(ap).protection_from_everything = false;
            // Expire "until your next turn" trigger and continuous instances for the active player.
            state.trigger_instances.retain(|ti| {
                !(ti.expiry == Some(Expiry::StartOfControllerNextTurn) && ti.controller == ap)
            });
            state.continuous_instances.retain(|ci| {
                !(ci.expiry == Expiry::StartOfControllerNextTurn && ci.controller == ap)
            });
        }
        StepKind::Draw => {
            let this_player_on_play = if ap == PlayerId::Us { on_play } else { !on_play };
            let skip = this_player_on_play && t == 1;
            if skip {
                state.log(t, ap, "No draw (on the play)");
            } else {
                sim_draw(state, ap, t, true);
            }
        }
        StepKind::Cleanup => {
            sim_discard_to_limit(state, t, ap);
            let cleanup_ids: Vec<ObjId> = state.permanents_of(ap).map(|c| c.id).collect();
            for id in cleanup_ids {
                if let Some(bf) = state.permanent_bf_mut(id) {
                    bf.damage = 0;
                }
            }
            // Expire EndOfTurn continuous and trigger instances.
            state.continuous_instances.retain(|ci| ci.expiry != Expiry::EndOfTurn);
            state.trigger_instances.retain(|ti| ti.expiry != Some(Expiry::EndOfTurn));
            // Expire unconsumed latent spell mods with EndOfTurn expiry.
            state.latent_spell_mods.retain(|lsm| lsm.expiry != Expiry::EndOfTurn);
        }
        StepKind::DeclareAttackers => {
            // Strategy decides who attacks and what each attacker targets.
            let decisions = state.with_strategy(ap, |s, st| s.declare_attackers(st));
            // Restriction abilities (e.g. Ensnaring Bridge) are legality gates,
            // not strategy hints. Drop any attacker forbidden by an active source.
            let decisions: Vec<(ObjId, Option<ObjId>)> = decisions.into_iter()
                .filter(|(atk_id, _)| {
                    let Some(obj) = state.objects.get(atk_id) else { return false };
                    if obj.controller != ap { return false; }
                    let Some(bf) = obj.bf() else { return false };
                    if bf.tapped || (bf.entered_this_turn && !creature_has_keyword(*atk_id, Keyword::Haste, state)) {
                        return false;
                    }
                    state.def_of(*atk_id).map_or(false, |d| d.types.contains(&CardType::Creature))
                        && !crate::ir::executor::action_restricted(
                            state, crate::ir::ability::ActionKind::Attack, *atk_id)
                })
                .collect();
            // Apply: mark each attacker on the battlefield. CR 702.20: vigilance skips the tap.
            for &(atk_id, target) in &decisions {
                let vigilant = creature_has_keyword(atk_id, Keyword::Vigilance, state);
                if let Some(bf) = state.permanent_bf_mut(atk_id) {
                    bf.attacking = true;
                    if !vigilant { bf.tapped = true; }
                    bf.attack_target = target;
                }
            }
            let attackers: Vec<ObjId> = decisions.iter().map(|&(id, _)| id).collect();
            if !attackers.is_empty() {
                let atk_descs: Vec<String> = attackers.iter().filter_map(|&atk_id| {
                    let p = state.objects.get(&atk_id)?;
                    let target_name = p.bf()?.attack_target
                        .and_then(|id| state.permanent_name(id))
                        .unwrap_or_else(|| "player".to_string());
                    Some(format!("{} → {}", p.catalog_key, target_name))
                }).collect();
                state.log(t, ap, format!("Declare attackers: {}", atk_descs.join(", ")));
            }
            state.combat_attackers = attackers.clone();
            // Fire triggers after attackers are marked.
            for atk_id in attackers {
                fire_event(GameEvent::CreatureAttacked {
                    attacker_id: atk_id,
                    attacker_controller: ap,
                }, state, t, ap);
            }
            fire_event(GameEvent::EnteredStep {
                step: StepKind::DeclareAttackers,
                active_player: ap,
            }, state, t, ap);
        }
        StepKind::DeclareBlockers => {
            let nap = ap.opp();
            // Strategy decides which blockers to assign.
            let blocks = state.with_strategy(ap.opp(), |s, st| s.declare_blockers(st));
            // Engine validation: drop illegal blocks (protection, etc.) as a safety net.
            let blocks: Vec<(ObjId, ObjId)> = blocks.into_iter()
                .filter(|&(atk_id, blk_id)| {
                    !creature_has_keyword(atk_id, Keyword::Unblockable, state)
                        && !is_protected_from(atk_id, blk_id, state)
                })
                .collect();
            for &(atk_id, blk_id) in &blocks {
                let atk_name = state.objects.get(&atk_id).map(|p| p.catalog_key.as_str()).unwrap_or("");
                let blk_name = state.objects.get(&blk_id).map(|p| p.catalog_key.clone()).unwrap_or_default();
                state.log(t, nap, format!("{} blocks {}", blk_name, atk_name));
            }
            // CR 509.1h: when an attacker is blocked by 2+ creatures, the attacking player
            // chooses the damage assignment order. Ask the attacker's strategy to reorder.
            let mut by_atk: HashMap<ObjId, Vec<ObjId>> = HashMap::new();
            for &(a, b) in &blocks { by_atk.entry(a).or_default().push(b); }
            let mut ordered: Vec<(ObjId, ObjId)> = Vec::with_capacity(blocks.len());
            // Preserve attacker order from `blocks` for stable output.
            let mut seen: std::collections::HashSet<ObjId> = Default::default();
            for &(a, _) in &blocks {
                if !seen.insert(a) { continue; }
                let bs = by_atk.remove(&a).unwrap_or_default();
                let final_order = if bs.len() >= 2 {
                    let chosen = state.with_strategy(ap, |s, st| s.order_blockers(st, a, &bs));
                    // Validate: must be a permutation of `bs`. Fall back to declaration order if not.
                    let in_set: std::collections::HashSet<ObjId> = bs.iter().copied().collect();
                    let out_set: std::collections::HashSet<ObjId> = chosen.iter().copied().collect();
                    if chosen.len() == bs.len() && in_set == out_set { chosen } else { bs }
                } else {
                    bs
                };
                for b in final_order { ordered.push((a, b)); }
            }
            state.combat_blocks = ordered;
            // Mark unblocked attackers so ninjutsu can target them.
            let blocked_atk_ids: std::collections::HashSet<ObjId> =
                state.combat_blocks.iter().map(|(a, _)| *a).collect();
            for &atk_id in &state.combat_attackers.clone() {
                if !blocked_atk_ids.contains(&atk_id) {
                    if let Some(bf) = state.permanent_bf_mut(atk_id) {
                        bf.unblocked = true;
                    }
                }
            }
        }
        StepKind::FirstStrikeCombatDamage => {
            do_combat_damage_pass(state, t, ap, DamageStepKind::FirstStrike);
        }
        StepKind::CombatDamage => {
            do_combat_damage_pass(state, t, ap, DamageStepKind::Regular);
        }
        StepKind::EndCombat => {
            state.combat_attackers.clear();
            state.combat_blocks.clear();
            let all_ids: Vec<ObjId> = state.objects.values()
                .filter(|c| c.in_zone(Zone::Battlefield))
                .map(|c| c.id)
                .collect();
            for id in all_ids {
                if let Some(bf) = state.permanent_bf_mut(id) {
                    bf.attacking = false;
                    bf.unblocked = false;
                }
            }
        }
        StepKind::Upkeep | StepKind::BeginCombat | StepKind::End => {
            // No automatic actions.
        }
    }

    // Fire EnteredStep for all priority-bearing steps.
    // DeclareAttackers fires it inside its own arm (after p.attacking is set) so skip it here.
    if step.prio && step.kind != StepKind::DeclareAttackers {
        let step_ev = GameEvent::EnteredStep {
            step: step.kind,
            active_player: ap,
        };
        fire_event(step_ev, state, t, ap);
    }

    if step.prio {
        handle_priority_round(state, t, ap);
    }
    // Mana pool drains at the end of every step.
    state.player_mut(PlayerId::Us).pool.drain();
    state.player_mut(PlayerId::Opp).pool.drain();
    check!(state.player(PlayerId::Us).pool.total == 0 && state.player(PlayerId::Opp).pool.total == 0, state, "step-end",
        "mana pool not empty after step drain");
}


/// Execute a full phase: run each step, then optionally run a phase-level priority round.
fn do_phase(
    state: &mut SimState,
    t: u8,
    ap: PlayerId,
    phase: &Phase,
    on_play: bool,
) {
    for step in &phase.steps {
        // CR 510.5: the first-strike combat damage step occurs only when at least
        // one attacking or blocking creature has first strike or double strike.
        if step.kind == StepKind::FirstStrikeCombatDamage
            && !any_combatant_has_first_or_double_strike(state)
        {
            continue;
        }
        do_step(state, t, ap, step, on_play);
        if state.done() {
            return;
        }
    }
    if phase.is_main_phase() {
        state.current_phase = Some(TurnPosition::Phase(phase.kind));
        let phase_ev = GameEvent::EnteredPhase { phase: phase.kind };
        fire_event(phase_ev, state, t, ap);
        // CR 714.2b: as a player's precombat main phase begins, they put a lore
        // counter on each Saga they control (firing the next chapter).
        if phase.kind == PhaseKind::PreCombatMain {
            let sagas: Vec<ObjId> = state.permanents_of(ap)
                .filter(|c| obj_is_saga(state, c.id))
                .map(|c| c.id)
                .collect();
            for saga_id in sagas {
                add_lore_counter(state, saga_id, t);
            }
        }
        handle_priority_round(state, t, ap);
        assert_engine_invariants(state, "phase-end");
        // Mana pool drains at the end of the main phase.
        state.player_mut(PlayerId::Us).pool.drain();
        state.player_mut(PlayerId::Opp).pool.drain();
        check!(state.player(PlayerId::Us).pool.total == 0 && state.player(PlayerId::Opp).pool.total == 0, state, "phase-end",
            "mana pool not empty after drain");
        if state.done() { return; }
    }
}

/// Simulate one full turn for the active player `ap`.
fn do_turn(
    state: &mut SimState,
    t: u8,
    ap: PlayerId,
    on_play: bool,
) {
    state.current_turn = t;
    state.current_ap = state.player_id(ap);
    state.event_log.mark_turn_start();
    do_phase(state, t, ap, &beginning_phase(), on_play);
    if state.done() { return; }

    do_phase(state, t, ap, &main_phase(), on_play);
    if state.done() { return; }

    do_phase(state, t, ap, &combat_phase(), on_play);
    if state.done() { return; }

    do_phase(state, t, ap, &post_combat_main_phase(), on_play);
    if state.done() { return; }

    do_phase(state, t, ap, &end_phase(), on_play);
}


/// Simulate the full game up to the Doomsday turn.
/// Returns the final `SimState` — check `state.terminal` to see if Doomsday resolved.
/// Everything an application supplies to run one game on the engine's generic
/// loop: the two decks + decision policies + card evaluator + termination
/// objective + config. The engine owns the loop (shuffle, opening hands,
/// mulligans, turn order); the application owns *who* plays, *what* decks, *how*
/// cards are valued, and *when* the run ends (via the `Objective`). This is the
/// seam between the engine and concrete apps (libmtg-doomsday: goldfish, pile builder).
/// (pub(crate) for now — becomes `pub` when the apps are split into their own crates.)
pub struct Scenario {
    pub us_label: String,
    pub opp_label: String,
    pub catalog: HashMap<String, CardDef>,
    pub us_deck: Vec<(String, i32, String)>,
    pub opp_deck: Vec<(String, i32, String)>,
    pub us_strategy: Box<dyn Strategy>,
    pub opp_strategy: Box<dyn Strategy>,
    pub evaluate_card: Arc<dyn Fn(PlayerId, ObjId, &SimState) -> f64 + Send + Sync>,
    pub objective: Box<dyn crate::objective::Objective>,
    pub max_turns: u8,
    /// `Some(b)` forces the on-the-play coin; `None` flips it with `rng`.
    pub on_play: Option<bool>,
    /// `Some(cards)` forces US's opening 7 to exactly these catalog keys (no mulligan,
    /// no London bottom) — the rest of the deck is the shuffled library. For estimating
    /// a specific hand's P(win) by replaying it under many shuffles. `None` = deal normally.
    pub fixed_us_hand: Option<Vec<String>>,
}

/// Run one game to completion on the generic engine loop. Application-agnostic:
/// every app-specific behavior arrives via `scenario`. Returns the final state —
/// inspect `state.terminal` (and the objective) for the outcome.
pub fn run_game(scenario: Scenario, rng: &mut impl Rng) -> SimState {
    let Scenario {
        us_label, opp_label, catalog, us_deck, opp_deck,
        us_strategy, opp_strategy, evaluate_card, objective, max_turns, on_play, fixed_us_hand,
    } = scenario;

    let on_play = on_play.unwrap_or_else(|| rng.gen_bool(0.5));
    let us = PlayerState::new(&us_label);
    let opp = PlayerState::new(&opp_label);
    let mut state = SimState::new(us, opp);
    // Derive every in-game random event (opening shuffles, random discard, etc.)
    // from the caller-provided RNG. Previously SimState::new() seeded this stream
    // from entropy, so two run_game calls with the same external seed were not
    // actually replayable. One derived seed keeps the engine deterministic while
    // leaving direct SimState::new() test construction unchanged.
    let simulation_seed: u64 = rng.gen();
    state.rng = Box::new(rand::rngs::StdRng::seed_from_u64(simulation_seed));
    state.catalog = catalog;
    state.on_play = on_play;

    // Populate state.objects with Library-zone objects for each player's mainboard.
    // catalog: game setup — ObjIds are assigned here for the first time; materialized
    // does not exist yet. Catalog is the only source of card definitions at this stage.
    // A name with no catalog entry is kept as a BLANK card: it occupies its deck
    // slot and is drawn normally, but has no def, so it is never castable/playable
    // (every `catalog.get()` consumer guards on `None` and skips it) — i.e. inert
    // air. This keeps deck size honest rather than silently shrinking the library.
    for (name, qty, board) in &us_deck {
        if board != "main" {
            // Forge simulator shortcut: materialize sideboard cards as face-up exile.
            // Karn, the Great Creator may retrieve any artifact you own from exile,
            // so this cleanly unifies wishboard cards with cards exiled in-game
            // (for example a One Ring exiled by Force of Negation).
            for _ in 0..*qty {
                let id = state.alloc_id();
                let mut obj = GameObject::new(id, name.clone(), PlayerId::Us);
                obj.set_zone(Zone::Exile { on_adventure: false });
                state.objects.insert(id, obj);
            }
            continue;
        }
        for _ in 0..*qty {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject::new(id, name.clone(), PlayerId::Us));
            state.player_mut(PlayerId::Us).library_order.push_back(id);
        }
    }
    for (name, qty, board) in &opp_deck {
        if board != "main" {
            for _ in 0..*qty {
                let id = state.alloc_id();
                let mut obj = GameObject::new(id, name.clone(), PlayerId::Opp);
                obj.set_zone(Zone::Exile { on_adventure: false });
                state.objects.insert(id, obj);
            }
            continue;
        }
        for _ in 0..*qty {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject::new(id, name.clone(), PlayerId::Opp));
            state.player_mut(PlayerId::Opp).library_order.push_back(id);
        }
    }

    // Shuffle both libraries.
    state.shuffle_library(PlayerId::Us);
    state.shuffle_library(PlayerId::Opp);

    // Install the application's card evaluator, decision policies, and objective.
    state.evaluate_card = evaluate_card;
    state.set_strategy(PlayerId::Us, us_strategy);
    state.set_strategy(PlayerId::Opp, opp_strategy);
    state.objective = Some(objective);

    // Deal opening hands with mulligan decisions (London mulligan).
    let mut mulligans = [0u32; 2];
    for (i, who) in [PlayerId::Us, PlayerId::Opp].into_iter().enumerate() {
        if who == PlayerId::Us {
            if let Some(ref hand) = fixed_us_hand {
                // Force US's opening 7 to exactly `hand`: move one library object per
                // requested name into hand; no mulligan, no London bottom.
                for name in hand {
                    let id = state.player(PlayerId::Us).library_order.iter().copied().find(|&id| {
                        state.objects.get(&id).map_or(false, |o| &o.catalog_key == name)
                    });
                    let id = id.unwrap_or_else(|| {
                        // The decklist doesn't contain this card. Rather than silently drop it
                        // (which would leave a < 7-card hand and quietly wrong stats), materialize
                        // it into the library exactly as deck-build does, so the forced hand is
                        // honored verbatim. The library still supplies the natural draws / tutor
                        // targets from the decklist.
                        let new_id = state.alloc_id();
                        state.objects.insert(
                            new_id,
                            GameObject::new(new_id, name.clone(), PlayerId::Us),
                        );
                        state.player_mut(PlayerId::Us).library_order.push_back(new_id);
                        new_id
                    });
                    state.set_card_zone(id, Zone::Hand { known: false });
                }
                state.player_mut(who).draws_this_turn = 0;
                continue;
            }
        }
        for _ in 0..7 { sim_draw(&mut state, who, 0, false); }
        loop {
            let taken = mulligans[i];
            let (take, decisions) = state.with_strategy(who, |s, st| {
                (s.take_mulligan(st, taken), s.drain_decisions())
            });
            state.decision_log.extend(decisions);
            if !take { break; }
            mulligans[i] += 1;
            if who == PlayerId::Us {
                // Snapshot the thrown-back hand for sample reporting.
                let h: Vec<String> = state.hand_of(who).map(|c| c.catalog_key.clone()).collect();
                state.mulliganed_hands_us.push(h);
            }
            // Return hand to library via set_card_zone (maintains library_order).
            let hand_ids: Vec<ObjId> = state.hand_of(who).map(|c| c.id).collect();
            for id in hand_ids { state.set_card_zone(id, Zone::Library); }
            // Shuffle library after returning cards.
            state.shuffle_library(who);
            state.player_mut(who).draws_this_turn = 0;
            // Draw 7 again (London mulligan: always see 7, then put N on bottom).
            for _ in 0..7 { sim_draw(&mut state, who, 0, false); }
        }
        // London bottom: put N cards from hand to bottom of library (N = mulligans taken).
        let n = mulligans[i] as usize;
        if n > 0 {
            let to_bottom = state.with_strategy(who, |s, st| s.london_bottom(st, n));
            for id in to_bottom {
                state.set_card_zone(id, Zone::Library);
                // set_card_zone pushes to back of library_order — that's already "bottom". Good.
            }
        }
        state.player_mut(who).draws_this_turn = 0;
    }

    // Snapshot the kept opening hand (post-mulligan) for sample reporting.
    state.opening_hand_us = state.hand_of(PlayerId::Us).map(|c| c.catalog_key.clone()).collect();

    let us_hand = state.hand_size(PlayerId::Us);
    let opp_hand = state.hand_size(PlayerId::Opp);
    state.log(
        0,
        PlayerId::Us,
        format!(
            "{} ({}) | us: {} cards (-{} mulligans), opp: {} cards (-{} mulligans)",
            opp_label,
            if on_play { "play" } else { "draw" },
            us_hand,
            mulligans[0],
            opp_hand,
            mulligans[1],
        ),
    );

    // ── Turn loop ────────────────────────────────────────────────────────────
    // Run until the game ends (objective fires, someone wins, etc.) or a hard cap.
    for t in 1..=max_turns {
        if !on_play {
            do_turn(&mut state, t, PlayerId::Opp, on_play);
            if state.done() { break; }
        }
        {
            do_turn(&mut state, t, PlayerId::Us, on_play);
            if state.done() { break; }
        }
        if on_play {
            do_turn(&mut state, t, PlayerId::Opp, on_play);
            if state.done() { break; }
        }
    }

    state
}

// ── Implementation checking ───────────────────────────────────────────────────

/// True if `def` has enough simulation implementation to do something during a game.
///
/// - Lands are always actionable (played via land-drop logic).
/// - Permanents (creatures, artifacts, planeswalkers, enchantments) are always castable.
/// - Spells need a target (including stack targets), abilities, or effects in `build_spell_effect`.
fn card_has_implementation(def: &CardDef) -> bool {
    if def.is_land() { return true; }
    if !def.abilities().is_empty() { return true; }
    if !def.target_spec().is_none() { return true; }
    match &def.kind {
        CardKind::Creature(_) | CardKind::Artifact(_)
        | CardKind::Planeswalker(_) | CardKind::Enchantment(_) => true,
        CardKind::Instant(s) | CardKind::Sorcery(s) => s.modes.is_some() || def.mode_count().is_some(),
        CardKind::Land(_) => true,
    }
}

/// Why a deck card can't be faithfully simulated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum UnimplementedKind {
    /// ✗ name not in the catalog at all — kept in the deck as a blank card
    /// (drawn but never castable/playable), so deck size stays honest.
    Missing,
    /// ~ in the catalog but no actionable effects — drawn but never played/cast.
    Inert,
}

/// One deck card the engine can't simulate, and why. Serializable so the web
/// frontend can render it and build a pre-filled `missing-card` issue.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UnimplementedCard {
    pub name: String,
    pub qty: i32,
    /// `"main"` or `"side"`.
    pub board: String,
    pub kind: UnimplementedKind,
}

/// Classify a deck's cards against the catalog, returning every card the engine
/// can't simulate (missing or inert), in input order. The shared data behind both
/// [`warn_unimplemented_cards`] (CLI) and the web "file a missing-card issue" flow.
pub fn classify_unimplemented_cards(
    cards: &[(String, i32, String)],
    catalog: &HashMap<String, CardDef>,
) -> Vec<UnimplementedCard> {
    cards
        .iter()
        .filter_map(|(name, qty, board)| {
            let kind = match catalog.get(name.as_str()) {
                None => UnimplementedKind::Missing,
                Some(def) if !card_has_implementation(def) => UnimplementedKind::Inert,
                _ => return None,
            };
            Some(UnimplementedCard { name: name.clone(), qty: *qty, board: board.clone(), kind })
        })
        .collect()
}

/// Print a warning for deck cards that lack a simulation implementation.
///
/// Two categories, both kept in the deck (deck size stays honest) and inert:
///   ✗ not in catalog — kept as a blank card (drawn but never castable)
///   ~ in catalog but no actionable effects — drawn but never played/cast
pub fn warn_unimplemented_cards(
    cards: &[(String, i32, String)],
    deck_label: &str,
    catalog: &HashMap<String, CardDef>,
) {
    let report = classify_unimplemented_cards(cards, catalog);
    if report.is_empty() { return; }

    let emit = |c: &UnimplementedCard| match c.kind {
        UnimplementedKind::Missing =>
            println!("   ✗ {}×{} — not in catalog (kept as a blank card; drawn but never cast)", c.qty, c.name),
        UnimplementedKind::Inert =>
            println!("   ~ {}×{} — no simulation effects (drawn but never cast)", c.qty, c.name),
    };
    let on = |c: &&UnimplementedCard, board: &str, kind: UnimplementedKind|
        c.board == board && c.kind == kind;

    println!("\n⚠  {} — unimplemented cards:", deck_label);
    report.iter().filter(|c| on(c, "main", UnimplementedKind::Missing)).for_each(emit);
    report.iter().filter(|c| on(c, "main", UnimplementedKind::Inert)).for_each(emit);
    if report.iter().any(|c| c.board != "main") {
        println!("   sideboard:");
        report.iter().filter(|c| c.board != "main" && c.kind == UnimplementedKind::Missing).for_each(emit);
        report.iter().filter(|c| c.board != "main" && c.kind == UnimplementedKind::Inert).for_each(emit);
    }
}
