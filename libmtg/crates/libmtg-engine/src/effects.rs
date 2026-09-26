use std::sync::Arc;
use super::*;
use crate::ir::expr::Filter;

/// Actor-relative player reference used in effect primitives.
/// `Actor` = the spell's controller; `Opp` = their opponent; `Any` = either player —
/// a TARGET-controller relaxation (e.g. a counter that may hit your OWN spell, as in
/// "Daze your own Lotus Petal" to advance a per-turn spell count). `Any` is only meaningful
/// as the `controller` of an `ObjectInZone` target spec, where `legal_targets` special-cases
/// it; `resolve` keeps a total fallback for any stray non-target use.
#[derive(Clone, Copy, Debug)]
pub enum Who { Actor, Opp, Any }

impl Who {
    pub(crate) fn resolve(&self, actor: PlayerId) -> PlayerId {
        match self { Who::Actor => actor, Who::Opp => actor.opp(), Who::Any => actor }
    }
}

/// A composable game effect. Wraps a closure that mutates SimState.
/// Built from primitives (eff_draw, eff_destroy_target, etc.) and chained with `.then()`.
/// Effects that need randomness access `state.rng` directly inside their closure.
pub struct Effect(pub(crate) Arc<dyn Fn(&mut SimState, u8, &[ObjId]) + Send + Sync>);

impl Clone for Effect {
    fn clone(&self) -> Self { Effect(Arc::clone(&self.0)) }
}

impl Effect {
    pub(crate) fn call(
        &self,
        state: &mut SimState,
        t: u8,
        targets: &[ObjId],
    ) {
        (self.0)(state, t, targets);
    }

    /// Chain two effects: `self` runs first, then `next`.
    /// Test-only now — card spell bodies compose IR `Action::Sequence` instead;
    /// kept for direct-call test scaffolding (parity tests chain closure effects).
    #[allow(dead_code)]
    pub(crate) fn then(self, next: Effect) -> Effect {
        let a = self.0;
        let b = next.0;
        Effect(Arc::new(move |state, t, targets: &[ObjId]| {
            a(state, t, targets);
            b(state, t, targets);
        }))
    }
}

// ── Effect primitives ─────────────────────────────────────────────────────────

/// Run a self-contained IR `Action` with `who` as the acting controller, as a
/// legacy closure `Effect`. The transitional bridge that lets a still-closure
/// spell body compose IR primitives (`MayDo`, `Shuffle`, …) instead of
/// re-implementing them as bespoke closures.
pub(crate) fn eff_ir(who: PlayerId, action: crate::ir::action::Action) -> Effect {
    Effect(Arc::new(move |state, _t, _targets| {
        let env = crate::ir::executor::BindEnv::new().with_controller(who);
        crate::ir::executor::execute(&action, state, &env);
    }))
}

/// Draw `n` cards for `who`.
pub(crate) fn eff_draw(who: PlayerId, n: usize) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        for _ in 0..n {
            sim_draw(state, who, t, false);
        }
    }))
}

/// Surveil N for `who`. For each of the top N cards of their library, calls
/// `Strategy::surveil_choice` (via `with_strategy`) to decide keep-on-top or put-in-graveyard.
/// Reveal step is a no-op (hidden information is not modeled).
/// TODO: surveil N>1 for Kaito, Bane of Nightmares 0 ability (passes N cards as a batch).
pub(crate) fn eff_surveil(who: PlayerId, n: usize) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        for _ in 0..n {
            let top = state.library_of(who).next().map(|o| o.id);
            if let Some(id) = top {
                if state.with_strategy(who, |s, st| s.surveil_choice(id, st)) {
                    change_zone(id, ZoneId::Graveyard, state, t, who);
                }
            }
        }
        state.player_mut(who).known_top_len = 0; // reorders the top — forget conservatively
    }))
}

/// Evaluator-driven put-back: score hand cards and put the `n` lowest-scoring
/// on top of library. Calls `state.evaluate_card` to score each hand card.
#[allow(dead_code)]
pub(crate) fn eff_put_back(who: PlayerId, n: usize) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        for _ in 0..n {
            let eval = Arc::clone(&state.evaluate_card);
            let scored: Vec<(ObjId, f64)> = state.hand_of(who)
                .map(|c| (c.id, eval(who, c.id, state)))
                .collect();
            if let Some(&(worst_id, _)) = scored.iter()
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            {
                let name = state.objects.get(&worst_id).map(|o| o.catalog_key.clone()).unwrap_or_default();
                change_zone(worst_id, ZoneId::Library, state, t, who);
                let lib = match who {
                    PlayerId::Us  => &mut state.player_mut(PlayerId::Us).library_order,
                    PlayerId::Opp => &mut state.player_mut(PlayerId::Opp).library_order,
                };
                if lib.back() == Some(&worst_id) {
                    lib.pop_back();
                    lib.push_front(worst_id);
                }
                state.log(t, who, format!("puts back {}", name));
            }
        }
        // Put-back mixes known and unknown cards, and is frequently followed by a
        // shuffle (fetchland) — reset the ENTIRE hand to unknown, not just the
        // cards that were put back (we can't tell which ones remain).
        let remaining: Vec<ObjId> = state.hand_of(who).map(|c| c.id).collect();
        for id in remaining {
            if let Some(card) = state.objects.get_mut(&id) {
                card.set_zone(Zone::Hand { known: false });
            }
        }
        state.player_mut(who).known_top_len = 0; // mixes known/unknown — forget conservatively
    }))
}


/// Scry N: look at top N cards of `who`'s library. For each, if the evaluator scores
/// it above threshold (0.3), keep on top; otherwise put on bottom.
/// Cards kept on top retain their relative order; bottomed cards go to the bottom.
pub(crate) fn eff_scry(who: PlayerId, n: usize) -> Effect {
    Effect(Arc::new(move |state, _t, _targets| {
        let eval = Arc::clone(&state.evaluate_card);
        let lib = match who {
            PlayerId::Us  => &state.player(PlayerId::Us).library_order,
            PlayerId::Opp => &state.player(PlayerId::Opp).library_order,
        };
        let top_ids: Vec<ObjId> = lib.iter().take(n).copied().collect();
        if top_ids.is_empty() { return; }

        let mut keep_top = Vec::new();
        let mut send_bottom = Vec::new();
        for &id in &top_ids {
            let score = eval(who, id, state);
            if score >= 0.3 {
                keep_top.push(id);
            } else {
                send_bottom.push(id);
            }
        }
        // Remove the N cards from front of library, then re-insert:
        // kept cards go back to front (preserving order), bottomed cards go to back.
        let lib = match who {
            PlayerId::Us  => &mut state.player_mut(PlayerId::Us).library_order,
            PlayerId::Opp => &mut state.player_mut(PlayerId::Opp).library_order,
        };
        for _ in 0..top_ids.len().min(lib.len()) {
            lib.pop_front();
        }
        // Push kept cards back to front (in reverse to preserve order).
        for &id in keep_top.iter().rev() {
            lib.push_front(id);
        }
        // Push bottomed cards to back.
        for &id in &send_bottom {
            lib.push_back(id);
        }
        let kept = keep_top.len();
        let bottomed = send_bottom.len();
        state.log(0, who, format!("scry {} → {} top, {} bottom", n, kept, bottomed));
    }))
}

// `eff_order` (top-N library arrangement) was removed: it sorted by the engine's
// evaluator (a player-agency leak). It is now the IR `Action::OrderTop`, routed
// through `Strategy::order_top_library`.

/// `who` loses `n` life, with a log line.
/// Test-only now — card life-loss effects use the IR `Action::PayLife` primitive;
/// kept as the closure reference implementation for the `pay_life_parity` test.
#[allow(dead_code)]
pub(crate) fn eff_life_loss(who: PlayerId, n: i32) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        state.lose_life(who, n);
        let life = state.life_of(who);
        state.log(t, who, format!("→ lose {} life (now {})", n, life));
    }))
}

/// Add mana per `spec` (e.g. `"BBB"`) to `who`'s pool.
/// Fires `GameEvent::ManaProduced` so replacement effects can intercept.
pub(crate) fn eff_mana(who: PlayerId, spec: impl Into<String>) -> Effect {
    let spec = spec.into();
    Effect(Arc::new(move |state, t, _targets| {
        fire_event(GameEvent::ManaProduced { who, spec: spec.clone() }, state, t, who);
    }))
}

/// Deal `n` damage to a target — creature, planeswalker, or player (CR 120.2).
/// `source_id` identifies the damage source for protection checks (CR 702.16b).
/// Test-only now — card damage effects use the IR `Action::DealDamage` primitive
/// (which is also protection-aware); kept for direct-call test scaffolding.
#[allow(dead_code)]
pub(crate) fn eff_damage_target(caster: PlayerId, n: i32, source_id: ObjId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        let Some(&id) = targets.first() else { return; };
        if is_protected_from(id, source_id, state) {
            let name = state.objects.get(&id).map(|o| o.catalog_key.as_str()).unwrap_or("?");
            state.log(t, caster, format!("→ damage to {} prevented (protection)", name));
            return;
        }
        if id == state.us_id || id == state.opp_id {
            let who = state.who_pid(id);
            state.lose_life(who, n);
            state.log(t, caster, format!("→ deals {} damage to {}", n, who));
        } else {
            if let Some(bf) = state.permanent_bf_mut(id) {
                bf.damage += n;
            }
            let name = state.objects.get(&id).map(|o| o.catalog_key.as_str()).unwrap_or("?");
            state.log(t, caster, format!("→ deals {} damage to {}", n, name));
        }
    }))
}

/// Force `who` to sacrifice one permanent matching `filter`, chosen via `Strategy::sacrifice_choice`.
/// Models "sacrifice a [X] of your choice" (CR 701.16). The sacrificing player decides;
/// the effect moves the chosen permanent to the graveyard. No-ops if no match exists.
pub(crate) fn eff_sacrifice(caster: PlayerId, who: Who, filter: Filter) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        let target_who = who.resolve(caster);
        let env = crate::ir::executor::BindEnv::new().with_controller(target_who);
        let candidates: Vec<ObjId> = state.permanents_of(target_who)
            .filter(|o| o.bf().is_some() && crate::ir::executor::matches(&filter, o.id, state, &env))
            .map(|o| o.id)
            .collect();
        if candidates.is_empty() { return; }
        let chosen = state.with_strategy(target_who, |s, st| s.sacrifice_choice(target_who, &candidates, st));
        if let Some(id) = chosen {
            let name = state.objects.get(&id).map(|o| o.catalog_key.clone()).unwrap_or_default();
            state.log(t, caster, format!("→ {} sacrificed", name));
            change_zone(id, ZoneId::Graveyard, state, t, caster);
        }
    }))
}

/// Core "destroy" action for a single permanent. The future home for indestructibility checks.
/// Use this (not `change_zone`) wherever the rules say a permanent is "destroyed".
pub(crate) fn destroy_one(id: ObjId, state: &mut SimState, t: u8, actor: PlayerId) {
    if state.def_of(id).map_or(false, |d| d.indestructible) {
        let name = state.objects.get(&id).map(|o| o.catalog_key.clone()).unwrap_or_default();
        state.log(t, actor, format!("{} is indestructible", name));
        return;
    }
    change_zone(id, ZoneId::Graveyard, state, t, actor);
}

/// Destroy the permanent in `targets[0]`. Test-only now — card "destroy target"
/// effects use the IR `Action::Destroy` primitive (which also routes through
/// `destroy_one`); kept for direct-call test scaffolding.
#[allow(dead_code)]
pub(crate) fn eff_destroy_target(caster: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            destroy_one(id, state, t, caster);
        }
    }))
}

/// Exile the permanent in `targets[0]`.
#[allow(dead_code)]
pub(crate) fn eff_exile_target(caster: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            change_zone(id, ZoneId::Exile, state, t, caster);
        }
    }))
}

/// Bounce the permanent in `targets[0]` to its controller's hand.
pub(crate) fn eff_bounce_target(caster: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            change_zone(id, ZoneId::Hand, state, t, caster);
        }
    }))
}

/// Move the card in `targets[0]` onto the Battlefield.
/// Target selection happens in the strategy layer via `choose_spell_target`.
pub(crate) fn eff_reanimate(actor: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            change_zone(id, ZoneId::Battlefield, state, t, actor);
        }
    }))
}

/// Mark all cards in `target`'s hand as known (visible to the other player).
/// Models "Target player reveals their hand" oracle text (CR 701.16).
/// Test-only now — card reveal-hand effects use the IR `Action::Reveal` primitive;
/// kept as the closure reference implementation for direct-call test scaffolding.
#[allow(dead_code)]
pub(crate) fn eff_reveal_hand(caster: PlayerId, target: Who) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        let target_who = target.resolve(caster);
        let ids: Vec<ObjId> = state.hand_of(target_who).map(|c| c.id).collect();
        let names: Vec<String> = ids.iter()
            .filter_map(|id| state.objects.get(id))
            .map(|c| c.catalog_key.clone())
            .collect();
        for id in &ids {
            if let Some(card) = state.objects.get_mut(id) {
                card.set_zone(Zone::Hand { known: true });
            }
        }
        if !names.is_empty() {
            state.log(t, caster, format!("reveals hand: {}", names.join(", ")));
        }
    }))
}

/// Discard `n` random cards from `target`'s hand matching `filter`.
/// Test-only now — card discard effects use the IR `Action::Discard` primitive;
/// kept as the closure reference implementation for direct-call test scaffolding.
#[allow(dead_code)]
pub(crate) fn eff_discard(caster: PlayerId, target: Who, n: usize, filter: Filter) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        use rand::Rng;
        let target_who = target.resolve(caster);
        let env = crate::ir::executor::BindEnv::new().with_controller(target_who);
        for _ in 0..n {
            let candidates: Vec<ObjId> = state.hand_of(target_who)
                .filter(|c| crate::ir::executor::matches(&filter, c.id, state, &env))
                .map(|c| c.id)
                .collect();
            if candidates.is_empty() { break; }
            let id = candidates[state.rng.gen_range(0..candidates.len())];
            change_zone(id, ZoneId::Graveyard, state, t, caster);
        }
    }))
}

/// Put `card_name` onto the battlefield as a permanent for `owner`. Fires ETB triggers.
pub(crate) fn eff_enter_permanent(
    owner: PlayerId,
    card_name: impl Into<String>,
) -> Effect {
    let card_name = card_name.into();
    Effect(Arc::new(move |state, t, _targets| {
        let new_id = state.alloc_id();
        // Pre-register and immediately activate instances before the event fires,
        // so ETB replacement checks (e.g. Murktide self-ETB) can intercept the event.
        state.objects.insert(new_id, GameObject {
            id: new_id,
            catalog_key: card_name.clone(),
            owner,
            controller: owner,
            is_token: false,
            role: ObjectRole::Battlefield(BattlefieldState {
                entered_this_turn: true,
                ..BattlefieldState::new()
            }),
            materialized: None,
            counters: HashMap::new(), ci_timestamp: 0,
        });
        fire_event(
            GameEvent::ZoneChange {
                id: new_id,
                actor: owner,
                from: ZoneId::Stack,
                to: ZoneId::Battlefield,
                controller: owner,
            },
            state, t, owner,
        );
        state.log(t, owner, format!("{} enters play", card_name));
    }))
}

/// Counter a single spell or ability by id. Called by `eff_counter_target` and
/// Lavinia-style triggers that capture the spell id at trigger time.
/// Fizzles gracefully if the id is no longer on the stack.
pub(crate) fn counter_one(id: ObjId, state: &mut SimState, t: u8, actor: PlayerId) {
    let pos = state.stack.iter().position(|&sid| sid == id);
    if let Some(pos) = pos {
        // Prohibition gate: "can't be countered" CE effects (CR 614.17).
        // Only fires for spell objects; triggered abilities on the stack are not spells.
        if state.objects.contains_key(&id) {
            let spell_caster = state.objects[&id].controller;
            if fire_event(GameEvent::SpellBeingCountered { caster: spell_caster, card_id: id }, state, t, actor) {
                return;
            }
            // Check materialized prohibition_defs (CE-granted "can't be countered").
            // Mirrors how fire_triggers reads granted_trigger_defs from materialized defs.
            let mat_prohibited = state.def_of(id).map_or(false, |d| {
                let event = GameEvent::SpellBeingCountered { caster: spell_caster, card_id: id };
                d.prohibition_defs.iter().any(|p| (p.check)(&event, id, spell_caster, state))
            });
            if mat_prohibited {
                let name = state.stack_item_display_name(id).to_string();
                state.log(t, actor, format!("→ fizzled ({} can't be countered)", name));
                return;
            }
        }
        // Check counterable property before removing (CR 608.2b).
        let is_ability = state.objects.get(&id).map_or(false, |o| o.ability().is_some());
        let can_counter = if is_ability {
            state.objects.get(&id).and_then(|o| o.ability())
                .map_or(true, |ab| ab.counterable)
        } else if state.objects.contains_key(&id) {
            state.def_of(id)
                .or_else(|| state.objects.get(&id)
                    .and_then(|o| state.catalog.get(o.catalog_key.as_str())))
                .map_or(true, |d| d.counterable())
        } else {
            true
        };
        if !can_counter {
            let name = state.stack_item_display_name(id).to_string();
            state.log(t, actor, format!("→ fizzled ({} can't be countered)", name));
            return;
        }
        state.stack.remove(pos);
        if is_ability {
            // An ability that is countered ceases to exist (CR 608.2m).
            let name = state.stack_item_display_name(id).to_string();
            state.log(t, actor, format!("→ {} (ability) countered", name));
            state.objects.remove(&id);
        } else if state.objects.contains_key(&id) {
            let name = state.objects[&id].catalog_key.clone();
            state.log(t, actor, format!("→ {} countered", name));
            change_zone(id, ZoneId::Graveyard, state, t, actor);
            // `change_zone` → `set_zone(Graveyard)` already drops the spell payload,
            // so the countered spell carries no stale `SpellState` in the graveyard.
        } else {
            let ghost = state.objects.get(&id)
                .map(|c| format!("{} (zone={:?})", c.catalog_key, c.zone()))
                .unwrap_or_else(|| format!("obj#{}", id.0));
            state.log(t, actor, format!("→ fizzled (target {} not on stack)", ghost));
        }
    } else {
        let ghost = state.objects.get(&id)
            .map(|c| format!("{} (zone={:?})", c.catalog_key, c.zone()))
            .unwrap_or_else(|| format!("obj#{}", id.0));
        state.log(t, actor, format!("→ fizzled (target {} not on stack)", ghost));
    }
}

/// Counter the spell in `targets[0]` (a stack ObjId). Removes it from `state.stack` and
/// puts it in the owner's graveyard via `change_zone` (so replacement effects can intercept).
/// Fizzles if the target is no longer on the stack or if it can't be countered
/// (`CardDef::counterable == false` / `AbilityState::counterable == false`,
/// CR 608.2b — the spell was a legal target but the effect doesn't apply).
/// Test-only now — card counter effects use the IR `Action::Counter` primitive
/// (which also routes through `counter_one`); kept for direct-call test scaffolding.
#[allow(dead_code)]
pub(crate) fn eff_counter_target(caster: PlayerId) -> Effect {
    Effect(Arc::new(move |state, t, targets| {
        if let Some(&id) = targets.first() {
            counter_one(id, state, t, caster);
        }
    }))
}


/// Search `who`'s library for a card matching `predicate` and move it to `dest`.
/// `predicate` and `dest` are built at load time — no string dispatch at simulation time.
pub(crate) fn eff_fetch_search(
    who: PlayerId,
    predicate: Filter,
    dest: ZoneId,
) -> Effect {
    Effect(Arc::new(move |state, t, _targets| {
        // `matches` falls back to the catalog for unmaterialized library cards.
        let env = crate::ir::executor::BindEnv::new().with_controller(who);
        let candidates: Vec<ObjId> = state.library_of(who)
            .filter(|c| crate::ir::executor::matches(&predicate, c.id, state, &env))
            .map(|c| c.id)
            .collect();
        if !candidates.is_empty() {
            // CR 701.19: which card to find is the searching player's CHOICE, not a
            // random pick — route it through the strategy (`choose_for_effect`). The
            // fetch ability is still on the stack during its own resolution (CR 608.2m),
            // so the stack top is the source the strategy can use to recognize the
            // fetch and pick (e.g.) a black source. Default strategy picks the first.
            let src = state.stack.last().copied().unwrap_or_default();
            let chosen_id = state
                .with_strategy(who, |s, st| s.choose_for_effect(src, &candidates, st))
                .filter(|id| candidates.contains(id))
                .unwrap_or(candidates[0]);
            let name = state.objects.get(&chosen_id).map(|c| c.catalog_key.clone()).unwrap_or_default();
            state.log(t, who, format!("search → {}", name));
            change_zone(chosen_id, dest, state, t, who);
            // CR 701.19: shuffle library after searching.
            state.shuffle_library(who);
        }
    }))
}

/// Ward pay-or-counter effect (CR 702.20).
/// Offers `targeting_caster` the chance to pay `cost`; if they decline (or can't pay),
/// `targeting_spell` is countered. Called from Ward `TriggerContext` effects.
pub(crate) fn ward_pay_or_counter(
    ward_source: ObjId,
    cost: &crate::ir::action::Action,
    targeting_spell: ObjId,
    targeting_caster: PlayerId,
    ward_holder: PlayerId,
    state: &mut SimState,
    t: u8,
) {
    let schema_ok = crate::ir::cost_exec::build_schema(cost, state, targeting_caster, ward_source).is_some();
    let mana_ok = match crate::ir::ability::first_pay_mana_in_action(cost) {
        Some(mc) => state.potential_mana(targeting_caster).can_pay(&mc),
        None => true,
    };
    let can_pay = schema_ok && mana_ok;
    let will_pay = can_pay && {
        let decision = state.with_strategy(targeting_caster, |s, st|
            s.resolve_choice(ward_source, &ChoiceRequest::WardPayment { cost: cost.clone() }, st));
        matches!(decision, ChoiceResult::Bool(true))
    };
    if will_pay {
        let _ = crate::pay_ir_cost(state, t, targeting_caster, ward_source, cost, false);
        state.log(t, targeting_caster, "→ pays ward cost".to_string());
    } else {
        state.log(t, ward_holder, "→ ward: countering spell (cost not paid)".to_string());
        counter_one(targeting_spell, state, t, ward_holder);
    }
}
