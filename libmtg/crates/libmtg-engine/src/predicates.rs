use super::*;

// ── IR Filter combinators ─────────────────────────────────────────────────────
//
// One filter language. An IR `Filter(Expr)` is the inspectable, no-closure form
// of "does this object match?" — the same question `CardPredicate`/`ObjPredicate`
// answered. Evaluated by `ir::executor::matches(filter, id, state, env)`, which
// falls back to the catalog for unmaterialized library cards, so these work for
// targeting, search (library), CE conditions, and cost filters alike.
//
// Composable so card filters read declaratively: a green creature is
// `ir_and(ir_color(Green), ir_type(Creature))`.

use crate::ir::context::Ctx;
use crate::ir::expr::{Expr, Filter};

fn it() -> Expr { Expr::Ctx(Ctx::It) }

/// Matches everything.
pub(crate) fn ir_any() -> Filter { Filter(Expr::Bool(true)) }

/// `It == Source` — the candidate is the ability's own source. The idiom for a
/// self-scoped trigger ("when this enters / this attacks") or self-scoped CE.
pub(crate) fn ir_self() -> Filter {
    Filter(Expr::Eq(Box::new(it()), Box::new(Expr::Ctx(Ctx::Source))))
}

/// `type ∈ Types(It)`.
pub(crate) fn ir_type(t: CardType) -> Filter {
    Filter(Expr::Contains(Box::new(Expr::TypeLit(t)), Box::new(Expr::Types(Box::new(it())))))
}

/// `supertype ∈ Supertypes(It)` (e.g. Basic, Legendary, Snow).
pub(crate) fn ir_supertype(s: Supertype) -> Filter {
    Filter(Expr::Contains(Box::new(Expr::SupertypeLit(s)), Box::new(Expr::Supertypes(Box::new(it())))))
}

/// `subtype ∈ Subtypes(It)` — covers creature/artifact/spell subtypes AND land
/// types (island, swamp, …), which `Subtypes(It)` surfaces lowercased.
pub(crate) fn ir_subtype(s: &str) -> Filter {
    Filter(Expr::Contains(Box::new(Expr::SubtypeLit(s.to_string())), Box::new(Expr::Subtypes(Box::new(it())))))
}

/// A token.
pub(crate) fn ir_token() -> Filter { Filter(Expr::IsToken(Box::new(it()))) }

/// Evaluate a `Filter` against object `id` with `It`/`Source` = `id` and the
/// controller bound to `id`'s controller. The standard way to check a filter
/// that gates on a specific object (ability conditions, protection sources).
pub fn obj_matches(filter: &Filter, id: ObjId, state: &SimState) -> bool {
    let controller = state.objects.get(&id).map(|o| o.controller).unwrap_or(PlayerId::Us);
    let env = crate::ir::executor::BindEnv::new().with_source(id).with_controller(controller);
    crate::ir::executor::matches(filter, id, state, &env)
}

/// `color ∈ Colors(It)`.
pub(crate) fn ir_color(c: Color) -> Filter {
    Filter(Expr::Contains(Box::new(Expr::ColorLit(c)), Box::new(Expr::Colors(Box::new(it())))))
}

/// `ZoneOf(It) == z` — the zone an object is in. Fundamental: "target creature"
/// is `ir_and(ir_zone(Battlefield), ir_type(Creature))`, "counter target spell"
/// is `ir_and(ir_zone(Stack), …)`, graveyard targeting is `ir_zone(Graveyard)`.
pub(crate) fn ir_zone(z: ZoneId) -> Filter {
    Filter(Expr::Eq(Box::new(Expr::ZoneOf(Box::new(it()))), Box::new(Expr::ZoneLit(z))))
}

/// Colorless (no colored pips): `|Colors(It)| == 0`.
pub(crate) fn ir_colorless() -> Filter {
    Filter(Expr::Eq(Box::new(Expr::Count(Box::new(Expr::Colors(Box::new(it()))))), Box::new(Expr::Num(0))))
}

/// `keyword ∈ Keywords(It)`.
pub(crate) fn ir_keyword(kw: Keyword) -> Filter {
    Filter(Expr::Contains(Box::new(Expr::KeywordLit(kw)), Box::new(Expr::Keywords(Box::new(it())))))
}

/// `Mv(It) <= n`.
pub(crate) fn ir_mv_le(n: i32) -> Filter {
    ir_mv_le_expr(Expr::Num(n as i64))
}

/// `Mv(It) <= e` — like `ir_mv_le` but the bound is a runtime `Expr`, e.g. an
/// announced X bound under `Ctx::Var("x")` (Meltdown: "MV X or less").
pub(crate) fn ir_mv_le_expr(e: Expr) -> Filter {
    Filter(Expr::Le(Box::new(Expr::Mv(Box::new(it()))), Box::new(e)))
}

/// `Mv(It) == e` — the runtime-bounded MV-equality filter (e.g. Engineered
/// Explosives: nonland permanents whose MV equals its charge counters).
pub(crate) fn ir_mv_eq_expr(e: Expr) -> Filter {
    Filter(Expr::Eq(Box::new(Expr::Mv(Box::new(it()))), Box::new(e)))
}

/// A creature with `Toughness(It) <= n`.
pub(crate) fn ir_toughness_le(n: i32) -> Filter {
    ir_and(
        ir_type(CardType::Creature),
        Filter(Expr::Le(Box::new(Expr::Toughness(Box::new(it()))), Box::new(Expr::Num(n as i64)))),
    )
}

/// Has at least one counter of the given type.
pub(crate) fn ir_has_counter(ct: CounterType) -> Filter {
    Filter(Expr::Gt(Box::new(Expr::CountersOn(Box::new(it()), ct)), Box::new(Expr::Num(0))))
}

/// A colored spell on the stack — protection-source filter (Emrakul).
pub(crate) fn ir_colored_spell() -> Filter {
    ir_and(ir_spell(), ir_not(ir_colorless()))
}

/// A card-less ability on the stack (activated or triggered). Mana abilities never
/// reach the stack (CR 605.3a), so on the stack this matches exactly the abilities a
/// counter can target.
pub(crate) fn ir_ability() -> Filter { Filter(Expr::IsAbility(Box::new(it()))) }

/// A *triggered* ability (false for activated abilities and for non-abilities).
pub(crate) fn ir_triggered_ability() -> Filter { Filter(Expr::AbilityIsTriggered(Box::new(it()))) }

/// A spell: a card or copy on the stack — i.e. on the stack and NOT a card-less
/// ability (CR 111.1). The proper counterpart to `ir_ability()`; use this (not a bare
/// `ir_zone(Stack)`) for "counter target spell" so abilities aren't wrongly matched.
pub(crate) fn ir_spell() -> Filter {
    ir_and(ir_zone(ZoneId::Stack), ir_not(ir_ability()))
}

/// Conjunction.
pub(crate) fn ir_and(a: Filter, b: Filter) -> Filter {
    Filter(Expr::And(Box::new(a.0), Box::new(b.0)))
}

/// Disjunction.
pub(crate) fn ir_or(a: Filter, b: Filter) -> Filter {
    Filter(Expr::Or(Box::new(a.0), Box::new(b.0)))
}

/// Negation.
pub(crate) fn ir_not(a: Filter) -> Filter {
    Filter(Expr::Not(Box::new(a.0)))
}

// ── Protection ───────────────────────────────────────────────────────────────

/// True if `target_id` has protection from `source_id` (CR 702.16).
/// Checks each predicate in the target's `protection_from` against the source.
pub fn is_protected_from(target_id: ObjId, source_id: ObjId, state: &SimState) -> bool {
    let target_def = state.def_of(target_id)
        .or_else(|| state.objects.get(&target_id)
            .and_then(|o| state.catalog.get(o.catalog_key.as_str())));
    let prots = target_def.map(|td| td.protection_from.clone()).unwrap_or_default();
    prots.iter().any(|f| obj_matches(f, source_id, state))
}

/// CR 702.11b: a permanent with hexproof can't be the target of spells or abilities
/// an opponent controls. `source_controller` is whoever is activating/casting.
pub(crate) fn is_hexproof_from(target_id: ObjId, source_controller: PlayerId, state: &SimState) -> bool {
    let target_controller = state.objects.get(&target_id).map(|o| o.controller);
    if target_controller == Some(source_controller) { return false; } // can't hexproof yourself
    state.def_of(target_id).map_or(false, |d| {
        match &d.kind {
            CardKind::Creature(c) => c.keywords.contains(Keyword::Hexproof),
            _ => false,
        }
    })
}

/// Declarative description of what targets a spell or ability may choose from.
/// Used both to enumerate legal choices and to re-validate at resolution.
#[derive(Clone)]
pub enum TargetSpec {
    None,
    /// A specific player (`who` resolved relative to the acting controller).
    Player(Who),
    /// Any game object in `zone` controlled by `controller` matching `filter`.
    /// Covers permanents (Battlefield), spells (Stack), and cards in graveyard/library.
    ObjectInZone { controller: Who, zone: ZoneId, filter: Filter },
    /// Any one of several sub-specs is a legal target (e.g. "any target" = creature | planeswalker | player).
    Union(Vec<TargetSpec>),
}

impl TargetSpec {
    /// Returns true if this spec requires no target (i.e. `TargetSpec::None`).
    pub(crate) fn is_none(&self) -> bool { matches!(self, TargetSpec::None) }
}


/// Pick targets from a list of legal targets.
/// Single-target heuristic: prefer killable creature, then planeswalker/player
/// over non-killable creatures, then first available.
pub fn pick_targets(_spec: &TargetSpec, targets: &[ObjId], state: &SimState) -> Vec<ObjId> {
    if targets.is_empty() { return vec![]; }
    // Single-target heuristic
    // Prefer a killable creature
    if let Some(&id) = targets.iter().find(|&&id| {
        let is_creature = state.def_of(id)
            .or_else(|| state.objects.get(&id).and_then(|o| state.catalog.get(o.catalog_key.as_str())))
            .map(|d| d.is_creature()).unwrap_or(false);
        if !is_creature { return false; }
        let tgh = state.def_of(id)
            .or_else(|| state.objects.get(&id).and_then(|o| state.catalog.get(o.catalog_key.as_str())))
            .and_then(|d| d.as_creature()).map(|c| c.toughness()).unwrap_or(1);
        let dmg = state.permanent_bf(id).map(|bf| bf.damage).unwrap_or(0);
        tgh > 0 && tgh - dmg <= 1
    }) {
        return vec![id];
    }
    // Skip non-killable creatures — prefer planeswalker or player over them
    if let Some(&id) = targets.iter().find(|&&id| {
        !state.def_of(id)
            .or_else(|| state.objects.get(&id).and_then(|o| state.catalog.get(o.catalog_key.as_str())))
            .map(|d| d.is_creature()).unwrap_or(false)
    }) {
        return vec![id];
    }
    // Fallback: first target
    vec![targets[0]]
}

/// Produce a copy of `spec` whose object filters exclude `exclude_id`.
/// Used by IR-dispatched triggers to express "another target X" without letting
/// card-level filters close over a source ObjId they don't know.
pub(crate) fn exclude_from_target_spec(spec: &TargetSpec, exclude_id: ObjId) -> TargetSpec {
    match spec {
        TargetSpec::None => TargetSpec::None,
        TargetSpec::Player(w) => TargetSpec::Player(*w),
        TargetSpec::ObjectInZone { controller, zone, filter } => {
            // "another target X" = the original filter AND `It != exclude_id`.
            let not_excluded = ir_not(Filter(Expr::Eq(
                Box::new(it()),
                Box::new(Expr::ObjLit(exclude_id)),
            )));
            TargetSpec::ObjectInZone {
                controller: *controller,
                zone: *zone,
                filter: ir_and(filter.clone(), not_excluded),
            }
        }
        TargetSpec::Union(specs) => TargetSpec::Union(
            specs.iter().map(|s| exclude_from_target_spec(s, exclude_id)).collect(),
        ),
    }
}

/// Enumerate all legal targets for `spec` given the current game state.
/// No heuristic — returns every valid option. Caller picks.
pub fn legal_targets(spec: &TargetSpec, controller: PlayerId, source_id: ObjId, state: &SimState) -> Vec<ObjId> {
    match spec {
        TargetSpec::None => vec![],
        TargetSpec::Player(who) => {
            let p = who.resolve(controller);
            if state.player(p).protection_from_everything { vec![] }
            else { vec![state.player_id(p)] }
        },
        TargetSpec::ObjectInZone { controller: who, zone, filter } => {
            let any_controller = matches!(who, Who::Any);
            let target_who = if any_controller { None } else { Some(who.resolve(controller)) };
            let env = crate::ir::executor::BindEnv::new()
                .with_controller(controller)
                .with_source(source_id);
            objects_in_zone(zone, target_who, state)
                .filter(|&id| {
                    if *zone == ZoneId::Stack {
                        let actor_id = state.player_id(controller);
                        // A normal counter cannot target the actor's OWN spell; an Any-controller
                        // counter (e.g. dazing your own Lotus Petal for the spell count) may.
                        if !any_controller && state.stack_item_owner(id) == actor_id { return false; }
                        if !state.stack_item_is_counterable(id) { return false; }
                    }
                    // CR 702.16d: protection prevents targeting.
                    if is_protected_from(id, source_id, state) { return false; }
                    // CR 702.11b: hexproof prevents opponent targeting.
                    if is_hexproof_from(id, controller, state) { return false; }
                    crate::ir::executor::matches(filter, id, state, &env)
                })
                .collect()
        }
        TargetSpec::Union(specs) => {
            // Collect all legal targets from all sub-specs, deduplicating by id.
            let mut seen = std::collections::HashSet::new();
            let mut result = Vec::new();
            for sub in specs {
                for id in legal_targets(sub, controller, source_id, state) {
                    if seen.insert(id) {
                        result.push(id);
                    }
                }
            }
            result
        }
    }
}

/// Return true if at least one valid target exists for `spec`.
/// For stack targets, checks the current stack for opposing non-ability spells.
/// For permanent/zone targets, checks the battlefield or zone.
/// Returns false for `TargetSpec::None` (no target required = always valid; caller should check `is_none()` first).
/// Delegate to `legal_targets` so that the legality check used when presenting
/// actions is identical to the one used during casting/resolution (CR 601.2c).
pub fn has_valid_target(
    spec: &TargetSpec,
    state: &SimState,
    actor: PlayerId,
    source_id: ObjId,
) -> bool {
    !legal_targets(spec, actor, source_id, state).is_empty()
}



/// Iterate over ObjIds in the given zone controlled (or owned) by `who` — or by ANYONE
/// when `who` is `None` (a `Who::Any` target, e.g. a counter that may hit your own spell).
fn objects_in_zone<'a>(
    zone: &ZoneId,
    who: Option<PlayerId>,
    state: &'a SimState,
) -> impl Iterator<Item = ObjId> + 'a {
    let zone_card = match zone {
        ZoneId::Battlefield => Zone::Battlefield,
        ZoneId::Graveyard   => Zone::Graveyard,
        ZoneId::Stack       => Zone::Stack,
        ZoneId::Library     => Zone::Library,
        ZoneId::Exile       => Zone::Exile { on_adventure: false },
        ZoneId::Hand        => Zone::Hand { known: false },
    };
    state.objects.values()
        .filter(move |o| {
            let zone_match = match o.zone() {
                Some(Zone::Hand { .. }) => matches!(zone_card, Zone::Hand { .. }),
                Some(z) => z == zone_card,
                None => false,
            };
            zone_match && who.map_or(true, |w| o.controller == w || o.owner == w)
        })
        .map(|o| o.id)
}

